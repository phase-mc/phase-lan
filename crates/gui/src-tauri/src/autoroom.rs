//! Auto-room: "open the room by itself when the world is opened to the network".
//!
//! While there is no session and the setting is on, a background task listens for LAN announcements on this
//! PC (like the host's world search). When a world is found, Phase creates a room with the same
//! settings as the "Create room" button (the window sends them with the
//! [`set_auto_room`] command) and shows the "World open to friends" notification.
//!
//! If the host stopped the room themselves, that world (its port) is not opened again
//! automatically until it closes in the game: the next "Open to LAN" gets
//! a new port, and the auto-room fires again.

use std::{
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use tauri::{AppHandle, Emitter, Manager, State};

use crate::{
    codes,
    commands::{self, HostRequest},
    state::AppState,
    tray,
};

/// Event for the window: the auto-room created a room (the window prepares the new session's log).
pub const AUTO_ROOM_CHANNEL: &str = "phase://auto-room";
/// Pause between checks while a session is running or the setting is off.
const IDLE: Duration = Duration::from_secs(2);
/// How long to listen for announcements at a time (the game announces the world every 1.5 s).
const LISTEN: Duration = Duration::from_secs(4);
/// Port 4445 is unavailable — the next attempt comes no sooner than this.
const ERROR_PAUSE: Duration = Duration::from_secs(15);
/// The same world is not opened automatically more often than this (if the room failed to come up).
const RETRY_SAME_WORLD: Duration = Duration::from_secs(60);
/// How long to wait for the room to be ready for the notification.
const READY_WAIT: Duration = Duration::from_secs(90);
/// How long to wait for the room code for the notification (the code service answers immediately).
const CODE_WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct AutoRoom(Mutex<Inner>);

#[derive(Default)]
struct Inner {
    enabled: bool,
    /// Host request with the window's settings (the found port is substituted).
    template: Option<HostRequest>,
    /// The world whose room the host closed themselves.
    dismissed: Option<u16>,
    /// Last automatic attempt: world port and time.
    last_try: Option<(u16, Instant)>,
}

impl AutoRoom {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The host stopped the room of the world on this port themselves.
    pub fn dismiss(&self, port: u16) {
        self.lock().dismissed = Some(port);
    }

    fn template(&self) -> Option<HostRequest> {
        let inner = self.lock();
        inner.enabled.then(|| inner.template.clone()).flatten()
    }

    /// Whether the world on this port may be opened automatically; if so, records the attempt.
    fn try_world(&self, port: u16) -> bool {
        let mut inner = self.lock();
        if inner.dismissed == Some(port) {
            return false;
        }
        if inner
            .last_try
            .is_some_and(|(p, at)| p == port && at.elapsed() < RETRY_SAME_WORLD)
        {
            return false;
        }
        inner.last_try = Some((port, Instant::now()));
        true
    }

    /// No open world: worlds closed by the host are forgotten.
    fn world_closed(&self) {
        let mut inner = self.lock();
        inner.dismissed = None;
        inner.last_try = None;
    }
}

/// The window reports the setting and the host request (at startup and after every change).
#[tauri::command]
pub fn set_auto_room(auto: State<'_, AutoRoom>, enabled: bool, request: Option<HostRequest>) {
    let mut inner = auto.lock();
    inner.enabled = enabled;
    inner.template = request;
}

/// The auto-room background task.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(IDLE).await;
            let auto = app.state::<AutoRoom>();
            let state = app.state::<AppState>();
            if auto.template().is_none() || session_running(&state).await {
                continue;
            }
            let world = match phase_minecraft::lan::detect_local_world(LISTEN).await {
                Ok(world) => world,
                Err(err) => {
                    tracing::debug!("автокомната: поиск мира недоступен: {err:#}");
                    tokio::time::sleep(ERROR_PAUSE).await;
                    continue;
                }
            };
            let Some(world) = world else {
                auto.world_closed();
                continue;
            };
            let port = world.announcement.port;
            // While listening, the setting may have been turned off or a room created manually.
            let Some(mut request) = auto.template() else {
                continue;
            };
            if session_running(&state).await || !auto.try_world(port) {
                continue;
            }
            // The port was found by the program: if the host reopens the world on a different port,
            // the room moves there by itself (`phase_core` watches the world).
            request.found_port = Some(port);
            match commands::launch_host(&app, &state, request).await {
                Ok(status) => {
                    tracing::info!(
                        "автокомната: мир «{}» найден — открываю его друзьям",
                        world.announcement.motd
                    );
                    let _ = app.emit(AUTO_ROOM_CHANNEL, &status);
                    announce_when_ready(app.clone(), status.id);
                }
                Err(err) => tracing::debug!("автокомната не запущена: {err}"),
            }
        }
    });
}

async fn session_running(state: &AppState) -> bool {
    state
        .status()
        .await
        .is_some_and(|status| !status.phase.is_finished())
}

/// The "World open to friends" notification, once the room is ready.
fn announce_when_ready(app: AppHandle, id: u64) {
    tauri::async_runtime::spawn(async move {
        let deadline = Instant::now() + READY_WAIT;
        while Instant::now() < deadline {
            let status = app.state::<AppState>().status().await;
            let Some(status) = status.filter(|s| s.id == id && !s.phase.is_finished()) else {
                return;
            };
            if let Some(host) = &status.host {
                // The code appears a fraction of a second after the room is ready.
                let code = codes::settled(&app, id, CODE_WAIT)
                    .await
                    .and_then(|c| c.code);
                let body = match code {
                    Some(code) => phase_lang::tr!(
                        format!("Код комнаты: {code}. Скажите его друзьям."),
                        format!("Room code: {code}. Tell your friends.")
                    ),
                    None => phase_lang::tr!(
                        format!(
                            "«{}»: друзья могут заходить. Приглашение — в окне Phase.",
                            host.motd
                        ),
                        format!(
                            "“{}”: friends can join now. The invite is in the Phase window.",
                            host.motd
                        )
                    ),
                };
                tray::notify(
                    &app,
                    phase_lang::tr!("Мир открыт для друзей", "World open to friends"),
                    &body,
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}
