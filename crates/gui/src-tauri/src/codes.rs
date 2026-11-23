//! The short room code in the window: the host gets `K7F-2QX` and renews it while the room
//! is open; the guest enters the code instead of a long invitation.
//!
//! Where the "code → room" record lives: on your own code service if one is set (window,
//! `PHASE_CODES_URL`), otherwise in the open pkarr network (`phase_codes::pkarr`), without your own
//! server. If it doesn't answer, the window quietly shows the long invitation, as before.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use phase_codes::{
    client::{CodesClient, CodesError},
    pkarr::PkarrCodes,
};
use phase_core::SessionStatus;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::watch;

/// Event for the window: the room code changed (`null` — there is no code any more).
pub const ROOM_CODE_CHANNEL: &str = "phase://room-code";
/// Environment variable with the service address (checks, your own server).
pub const SERVICE_ENV: &str = "PHASE_CODES_URL";
/// The service didn't answer — the next attempt comes after this time.
const RETRY: Duration = Duration::from_secs(20);
/// How long to wait for the code to be released when the room closes.
const RELEASE_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeState {
    /// Registering the code.
    Pending,
    Ready,
    /// The service didn't answer: show the long invitation, try again.
    Unavailable,
    /// The code service is not configured (or "This PC only" mode).
    Off,
}

/// Room code of the current host session.
#[derive(Debug, Clone, Serialize)]
pub struct RoomCode {
    /// Session number (`SessionStatus::id`).
    pub session: u64,
    pub state: CodeState,
    pub code: Option<String>,
}

#[derive(Default)]
pub struct RoomCodes(Mutex<Option<RoomCode>>);

impl RoomCodes {
    pub fn current(&self) -> Option<RoomCode> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn set(&self, code: Option<RoomCode>) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = code;
    }
}

/// Room code for the window (after a page reload).
#[tauri::command]
pub fn room_code(codes: State<'_, RoomCodes>) -> Option<RoomCode> {
    codes.current()
}

/// Where to keep this session's codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Service {
    /// No codes ("This PC only" mode: no internet).
    Off,
    /// Your own code service (`phase-relay --codes`).
    Http(String),
    /// The open pkarr network, no server of your own needed.
    Pkarr,
}

/// Service: the address from the window settings, from the environment variable, the built-in one — or pkarr.
pub fn service(explicit: Option<&str>) -> Service {
    explicit
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(SERVICE_ENV)
                .ok()
                .map(|url| url.trim().to_string())
                .filter(|url| !url.is_empty())
        })
        .or_else(|| phase_codes::DEFAULT_SERVICE_URL.map(str::to_string))
        .map_or(Service::Pkarr, Service::Http)
}

/// Client of the chosen service.
enum Backend {
    Http(CodesClient),
    Pkarr(PkarrCodes),
}

/// Host code; `secret` exists only for your own service.
struct Held {
    code: String,
    secret: Option<String>,
}

impl Backend {
    fn new(service: Service) -> Result<Option<Self>, CodesError> {
        Ok(match service {
            Service::Off => None,
            Service::Http(url) => Some(Self::Http(CodesClient::new(&url)?)),
            Service::Pkarr => Some(Self::Pkarr(PkarrCodes::from_env()?)),
        })
    }

    async fn register(&self, ticket: &str) -> Result<Held, CodesError> {
        match self {
            Self::Http(client) => client.register(ticket).await.map(|reg| Held {
                code: reg.code,
                secret: Some(reg.secret),
            }),
            Self::Pkarr(codes) => codes
                .register(ticket)
                .await
                .map(|code| Held { code, secret: None }),
        }
    }

    async fn renew(&self, held: &Held, ticket: &str) -> Result<(), CodesError> {
        match (self, &held.secret) {
            (Self::Http(client), Some(secret)) => {
                client.renew(&held.code, secret, ticket).await.map(drop)
            }
            (Self::Pkarr(codes), _) => codes.publish(&held.code, ticket).await,
            (Self::Http(_), None) => Err(CodesError::Conflict),
        }
    }

    async fn release(&self, held: &Held) {
        let _ = match (self, &held.secret) {
            (Self::Http(client), Some(secret)) => client.release(&held.code, secret).await,
            (Self::Pkarr(codes), _) => codes.release(&held.code).await,
            (Self::Http(_), None) => Ok(()),
        };
    }

    async fn lookup(&self, code: &str) -> Result<String, CodesError> {
        match self {
            Self::Http(client) => client.lookup(code).await,
            Self::Pkarr(codes) => codes.lookup(code).await,
        }
    }
}

/// Guest: code → ticket. Not a code — the input is returned as is (it is a ticket).
pub async fn resolve(service: Service, input: &str) -> Result<String, String> {
    let Some(code) = phase_codes::normalize(input) else {
        return Ok(input.trim().to_string());
    };
    let fallback = phase_lang::tr!(
        "Попросите у друга длинное приглашение (оно под кодом комнаты) и вставьте его сюда.",
        "Ask your friend for the long invite (it is under the room code) and paste it here."
    );
    let Some(client) = Backend::new(service).map_err(|err| format!("{err}"))? else {
        return Err(phase_lang::tr!(
            format!("В режиме «Только этот ПК» коды комнат не работают. {fallback}"),
            format!("Room codes do not work in “This PC only” mode. {fallback}")
        ));
    };
    client.lookup(&code).await.map_err(|err| match err {
        CodesError::NotFound => phase_lang::tr!(
            format!(
                "Комнаты с кодом {code} нет. Проверьте код или попросите у друга новый: \
                 он меняется, если комнату закрыли."
            ),
            format!(
                "There is no room with code {code}. Check the code or ask your friend for a new one: \
                 it changes when the room is closed."
            )
        ),
        CodesError::RateLimited => phase_lang::tr!(
            "Слишком много попыток подряд. Подождите минуту и попробуйте снова.".to_string(),
            "Too many attempts in a row. Wait a minute and try again.".to_string()
        ),
        other => {
            tracing::warn!("сервис кодов не ответил: {other}");
            phase_lang::tr!(
                format!("Сервис кодов сейчас недоступен. {fallback}"),
                format!("The room code service is unavailable right now. {fallback}")
            )
        }
    })
}

/// Host: register the code when the room is ready, renew it and release it when
/// the room closes.
pub fn watch_host(app: AppHandle, service: Service, mut rx: watch::Receiver<SessionStatus>) {
    tauri::async_runtime::spawn(async move {
        // Wait for the ticket (the room is ready).
        let (id, ticket) = loop {
            let status = rx.borrow_and_update().clone();
            if status.phase.is_finished() {
                return;
            }
            if let Some(host) = status.host {
                break (status.id, host.ticket);
            }
            if rx.changed().await.is_err() {
                return;
            }
        };
        let client = Backend::new(service).unwrap_or_else(|err| {
            tracing::warn!("сервис кодов: {err}");
            None
        });
        let Some(client) = client else {
            publish(&app, id, CodeState::Off, None);
            return;
        };
        publish(&app, id, CodeState::Pending, None);

        let mut registered: Option<Held> = None;
        loop {
            let delay = match &registered {
                None => match client.register(&ticket).await {
                    Ok(held) => {
                        tracing::info!("код комнаты: {}", held.code);
                        publish(&app, id, CodeState::Ready, Some(held.code.clone()));
                        registered = Some(held);
                        phase_codes::RENEW_EVERY
                    }
                    Err(err) => {
                        tracing::info!("код комнаты не получен ({err}) — работает приглашение");
                        publish(&app, id, CodeState::Unavailable, None);
                        RETRY
                    }
                },
                Some(held) => match client.renew(held, &ticket).await {
                    Ok(()) => phase_codes::RENEW_EVERY,
                    // The code expired and went to another room — take a new one right away.
                    Err(CodesError::Conflict) => {
                        registered = None;
                        Duration::ZERO
                    }
                    // The network blinked: the code lives for up to 2 more minutes, retry sooner.
                    Err(err) => {
                        tracing::debug!("код комнаты не продлён: {err}");
                        RETRY
                    }
                },
            };
            if session_ended(&mut rx, delay).await {
                break;
            }
        }
        if let Some(held) = registered {
            let _ = tokio::time::timeout(RELEASE_WAIT, client.release(&held)).await;
        }
        let codes = app.state::<RoomCodes>();
        if codes.current().is_some_and(|c| c.session == id) {
            codes.set(None);
            let _ = app.emit(ROOM_CODE_CHANNEL, None::<RoomCode>);
        }
    });
}

/// Waits `delay`; `true` — the session ended in the meantime.
async fn session_ended(rx: &mut watch::Receiver<SessionStatus>, delay: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return false,
            changed = rx.changed() => {
                if changed.is_err() || rx.borrow().phase.is_finished() {
                    return true;
                }
            }
        }
    }
}

fn publish(app: &AppHandle, session: u64, state: CodeState, code: Option<String>) {
    let value = RoomCode {
        session,
        state,
        code,
    };
    app.state::<RoomCodes>().set(Some(value.clone()));
    let _ = app.emit(ROOM_CODE_CHANNEL, Some(value));
}

/// The room code of session `id`, once it becomes known (ready, unavailable or off);
/// `None` — not known within `within`.
pub async fn settled(app: &AppHandle, id: u64, within: Duration) -> Option<RoomCode> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(code) = app
            .state::<RoomCodes>()
            .current()
            .filter(|c| c.session == id && c.state != CodeState::Pending)
        {
            return Some(code);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}
