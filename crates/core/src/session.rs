//! A host or guest session: starts in the background, reports about itself through channels,
//! stops on request.
//!
//! ```text
//! Session::host(opts, bus) ──► background task (host::run)
//!     │  status()/watch_status() ◄── watch<SessionStatus>  (what is now)
//!     │  bus.subscribe()         ◄── broadcast<SessionEvent> (what happened)
//!     └─ stop() ──► CancellationToken ──► the task closes connections and finishes
//! ```

use std::{
    future::Future,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Result, anyhow};
use phase_tunnel::EndpointId;
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use crate::{
    events::{EventBus, Level},
    guest, host,
    look::LookCommand,
    options::{HostMods, HostOptions, JoinOptions},
    status::{Phase, Role, SessionStatus},
};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

/// What the guest asks to do with mods ([`Session::mod_command`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModCommand {
    /// Sync: extra mods go to the backup, missing ones are downloaded.
    Sync,
    /// Join without the host's mods (during a download, abort it).
    Skip,
    /// Check the folder against the host's mods again.
    Recheck,
    /// Return your own mods from the backup and remove the host's mods.
    Restore,
}

/// A command for a running session.
pub(crate) enum Control {
    /// Host: new mod distribution settings.
    HostMods(HostMods),
    /// Guest: an action with mods.
    Mods(ModCommand),
    /// Host: settings of the room's styling in the game.
    Look(LookCommand),
    /// Guest: turn the silhouette highlight on or off for themselves.
    Glow(bool),
    /// Host: remove a device from the allowed list. The reply says whether it was listed.
    Forget(EndpointId, oneshot::Sender<Result<bool>>),
}

/// Everything the session's background task needs to report about itself and to learn about a stop.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub bus: EventBus,
    status: watch::Sender<SessionStatus>,
    cancel: CancellationToken,
}

impl Ctx {
    pub fn update(&self, change: impl FnOnce(&mut SessionStatus)) {
        self.status.send_modify(|s| s.change(change));
    }

    pub fn phase(&self, phase: Phase) {
        self.update(|s| s.phase = phase);
    }

    pub fn info(&self, text: impl Into<String>) {
        self.bus.notice(Level::Info, text);
    }

    /// A warning: to the log (`tracing`) and to the user (an event).
    pub fn warn(&self, text: impl Into<String>) {
        let text = text.into();
        tracing::warn!("{text}");
        self.bus.notice(Level::Warn, text);
    }

    /// Runs `fut` until the session is asked to stop. `None` means a stop.
    pub async fn cancellable<T>(&self, fut: impl Future<Output = T>) -> Option<T> {
        self.cancel.run_until_cancelled(fut).await
    }
}

/// A running (or finished) session.
///
/// **Dropping the object stops the session** (connections are closed in the background). To
/// wait for a complete stop, use [`Session::shutdown`].
pub struct Session {
    id: u64,
    role: Role,
    status: watch::Sender<SessionStatus>,
    cancel: CancellationToken,
    control: mpsc::UnboundedSender<Control>,
    task: Option<JoinHandle<Result<()>>>,
}

impl Session {
    /// Starts the host in the background. The `tokio` runtime is required.
    pub fn host(opts: HostOptions, bus: EventBus) -> Self {
        let profile = opts.profile.clone();
        Self::spawn(Role::Host, profile, bus, move |ctx, control| {
            host::run(opts, ctx, control)
        })
    }

    /// Starts the guest in the background. The `tokio` runtime is required.
    pub fn join(opts: JoinOptions, bus: EventBus) -> Self {
        let profile = opts.profile.clone();
        Self::spawn(Role::Guest, profile, bus, move |ctx, control| {
            guest::run(opts, ctx, control)
        })
    }

    fn spawn<F, Fut>(role: Role, profile: String, bus: EventBus, run: F) -> Self
    where
        F: FnOnce(Ctx, mpsc::UnboundedReceiver<Control>) -> Fut,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let (status_tx, _) = watch::channel(SessionStatus::new(id, role, profile));
        let cancel = CancellationToken::new();
        let (control, control_rx) = mpsc::unbounded_channel();
        let ctx = Ctx {
            bus,
            status: status_tx.clone(),
            cancel: cancel.clone(),
        };
        let fut = run(ctx, control_rx);
        let status = status_tx.clone();
        let task = tokio::spawn(async move {
            let result = fut.await;
            status_tx.send_modify(|s| {
                s.change(|s| match &result {
                    Ok(()) => s.phase = Phase::Stopped,
                    Err(err) => {
                        s.phase = Phase::Failed;
                        s.error = Some(format!("{err:#}"));
                    }
                })
            });
            result
        });
        Self {
            id,
            role,
            status,
            cancel,
            control,
            task: Some(task),
        }
    }

    /// Host: change mod distribution without closing the room (turn on, turn off, another
    /// set or folder). Guests will see the new list at the next check. `false` means this is
    /// not a host session or it is already ending.
    pub fn set_host_mods(&self, mods: HostMods) -> bool {
        self.role == Role::Host && self.control.send(Control::HostMods(mods)).is_ok()
    }

    /// Guest: an action with mods. The result is in [`crate::GuestReady::mods`]. `false` means this is not
    /// a guest session or it is already ending.
    pub fn mod_command(&self, command: ModCommand) -> bool {
        self.role == Role::Guest && self.control.send(Control::Mods(command)).is_ok()
    }

    /// Host: change the room's styling in the game (settings or one player's style) without
    /// closing the room. The result is in [`crate::HostReady::look`]. `false` means this is not a
    /// host session or it is already ending.
    pub fn look_command(&self, command: LookCommand) -> bool {
        self.role == Role::Host && self.control.send(Control::Look(command)).is_ok()
    }

    /// Guest: turn the silhouette highlight on or off for themselves. The host decides: the result is in
    /// [`crate::GuestReady::look`]. `false` means this is not a guest session or it is already ending.
    pub fn set_own_glow(&self, glow: bool) -> bool {
        self.role == Role::Guest && self.control.send(Control::Glow(glow)).is_ok()
    }

    /// Host: remove a device from the allowed list without closing the room. The room keeps
    /// the list in memory and writes the profile file itself, so editing the file behind its
    /// back would not stick: the next save would bring the device back. `None` means this is
    /// not a host session or it is shutting down; otherwise the reply comes once the room
    /// has handled the command.
    pub fn forget_device(&self, id: EndpointId) -> Option<oneshot::Receiver<Result<bool>>> {
        if self.role != Role::Host {
            return None;
        }
        let (tx, rx) = oneshot::channel();
        self.control.send(Control::Forget(id, tx)).ok()?;
        Some(rx)
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// State now.
    pub fn status(&self) -> SessionStatus {
        self.status.borrow().clone()
    }

    /// Subscription to state changes.
    pub fn watch_status(&self) -> watch::Receiver<SessionStatus> {
        self.status.subscribe()
    }

    /// The session finished (by itself because of an error, or after a stop).
    pub fn is_finished(&self) -> bool {
        self.status.borrow().phase.is_finished()
    }

    /// Ask the session to stop (does not wait for the stop).
    pub fn stop(&self) {
        self.cancel.cancel();
    }

    /// Waits for the session to finish. An error says why the session finished.
    ///
    /// The wait can be interrupted (for example, in `select!`) and started again: the session does not
    /// stop because of it and the result is not lost.
    pub async fn wait(&mut self) -> Result<()> {
        if let Some(task) = self.task.as_mut() {
            let joined = task.await;
            self.task = None;
            return match joined {
                Ok(result) => result,
                Err(err) => {
                    let error = phase_lang::tr!(
                        format!("сессия аварийно завершилась: {err}"),
                        format!("the session crashed: {err}")
                    );
                    // The task did not manage to record the result: we record it ourselves.
                    self.status.send_modify(|s| {
                        s.change(|s| {
                            s.phase = Phase::Failed;
                            s.error = Some(error.clone());
                        })
                    });
                    Err(anyhow!(error))
                }
            };
        }
        match &self.status.borrow().error {
            Some(error) => Err(anyhow!(error.clone())),
            None => Ok(()),
        }
    }

    /// Stop and wait: connections are closed, the report is saved.
    pub async fn shutdown(mut self) -> Result<()> {
        self.stop();
        self.wait().await
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
