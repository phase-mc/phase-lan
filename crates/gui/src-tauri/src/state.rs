//! Global state of the app window (Tauri State).
//!
//! - One [`EventBus`] for the whole program: its events are forwarded to the window
//!   (`core://event`) by a separate task, whichever session is running.
//! - At most one session (host **or** guest) at a time. The session is kept under
//!   an async `tokio::sync::Mutex`: commands wait on it in background threads, the window
//!   thread is never blocked.
//! - The latest log lines are kept in a ring buffer, so the window can show them
//!   after a page reload.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, bail};
use phase_core::{ApprovalQueue, EventBus, Session, SessionStatus, logs::LogLine};
use tauri::{AppHandle, Emitter};

/// Event channels for the window (names match `src/lib/core.ts`).
pub const EVENT_CHANNEL: &str = "core://event";
pub const STATUS_CHANNEL: &str = "core://status";
pub const LOG_CHANNEL: &str = "core://log";

/// How many log lines to keep for the window.
const LOG_KEEP: usize = 500;
/// How long to wait for the session to stop when the program closes.
pub const EXIT_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The latest log lines.
#[derive(Clone, Default)]
pub struct LogBuffer(Arc<Mutex<VecDeque<LogLine>>>);

impl LogBuffer {
    pub fn push(&self, line: LogLine) {
        let mut lines = self.0.lock().expect("log buffer lock");
        if lines.len() == LOG_KEEP {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    pub fn snapshot(&self) -> Vec<LogLine> {
        self.0
            .lock()
            .expect("log buffer lock")
            .iter()
            .cloned()
            .collect()
    }
}

pub struct AppState {
    pub bus: EventBus,
    /// "Let them in?" questions from the host core; the window answers with the `answer_approval` command.
    pub approvals: ApprovalQueue,
    pub logs: LogBuffer,
    session: tokio::sync::Mutex<Option<Session>>,
}

impl AppState {
    pub fn new(bus: EventBus, logs: LogBuffer) -> Self {
        Self {
            approvals: ApprovalQueue::new(bus.clone()),
            bus,
            logs,
            session: tokio::sync::Mutex::new(None),
        }
    }

    /// Starts a new session if nothing is running now. Session state changes
    /// are forwarded to the window (`core://status`).
    pub async fn start(
        &self,
        app: &AppHandle,
        start: impl FnOnce(EventBus) -> Session,
    ) -> Result<SessionStatus> {
        let mut slot = self.session.lock().await;
        if let Some(current) = slot.as_ref()
            && !current.is_finished()
        {
            bail!(phase_lang::tr!(
                "уже работает другая сессия — сначала остановите её",
                "another session is already running: stop it first"
            ));
        }
        let session = start(self.bus.clone());
        forward_status(app.clone(), &session);
        let status = session.status();
        // The previous (finished) session is dropped here.
        *slot = Some(session);
        Ok(status)
    }

    /// Stops the session and waits for connections to close and the report to be saved.
    /// Returns the final state (`None` — there was no session).
    pub async fn stop(&self) -> Option<SessionStatus> {
        let mut slot = self.session.lock().await;
        let session = slot.as_mut()?;
        session.stop();
        // The session error is already recorded in its state — return that.
        let _ = session.wait().await;
        Some(session.status())
    }

    /// Subscription to the state of session number `id` (`None` — another one has replaced it).
    pub async fn watch(&self, id: u64) -> Option<tokio::sync::watch::Receiver<SessionStatus>> {
        self.session
            .lock()
            .await
            .as_ref()
            .map(Session::watch_status)
            .filter(|rx| rx.borrow().id == id)
    }

    pub async fn status(&self) -> Option<SessionStatus> {
        self.session.lock().await.as_ref().map(Session::status)
    }

    /// Access to the running session (for example, the mod sync command). `None` —
    /// there is no session or it has already finished.
    pub async fn with_active<T>(&self, action: impl FnOnce(&Session) -> T) -> Option<T> {
        self.session
            .lock()
            .await
            .as_ref()
            .filter(|session| !session.is_finished())
            .map(action)
    }
}

/// Forwards session state to the window: the current one right away, then every change until
/// the session ends (the final state — "stopped" or "error" — also
/// reaches the window).
fn forward_status(app: AppHandle, session: &Session) {
    let mut rx = session.watch_status();
    tauri::async_runtime::spawn(async move {
        loop {
            let status = rx.borrow_and_update().clone();
            if let Err(err) = app.emit(STATUS_CHANNEL, &status) {
                tracing::debug!("состояние сессии не отправлено в окно: {err}");
            }
            if status.phase.is_finished() || rx.changed().await.is_err() {
                break;
            }
        }
    });
}
