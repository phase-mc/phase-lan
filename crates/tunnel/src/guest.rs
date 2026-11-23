//! Guest: a local "twin" of the server on `127.0.0.1` and automatic
//! reconnection to the host.

use std::{
    fmt,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use iroh::{
    Endpoint, EndpointAddr,
    endpoint::{Connection, ConnectionError, VarInt},
};
use n0_future::task::AbortOnDropHandle;
use phase_lang::tr;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
};
use tracing::{info, warn};

use crate::{
    access::APPROVE_TIMEOUT,
    events::TunnelEvent,
    forward,
    metrics::{self, Telemetry},
    voice::{self, BoundVoicePort, VoicePorts, VoiceStats},
    wire::{self, Hello, HostInfo, RejectReason, StreamKind, code},
};

/// How long to wait for the host's answer when connecting (including the time the host decides
/// whether to let us in).
const HANDSHAKE_TIMEOUT: Duration = APPROVE_TIMEOUT.saturating_add(Duration::from_secs(30));
/// If the game connects while the connection to the host is being restored, how long to wait.
const WAIT_FOR_HOST: Duration = Duration::from_secs(5);
/// Maximum pause between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(10);
/// How many of the host's voice UDP ports the guest is willing to open locally.
pub const MAX_VOICE_PORTS: usize = 8;

/// The host denied access. Reconnecting is pointless until the host approves.
#[derive(Debug)]
pub struct NotAllowed {
    /// Reason from the host (`None` means the host sent an unknown reason).
    pub reason: Option<RejectReason>,
}

impl fmt::Display for NotAllowed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.reason {
            Some(reason) => write!(
                f,
                "{}: {}",
                tr!(
                    "хост не разрешил подключение",
                    "the host did not allow the connection"
                ),
                reason.explain()
            ),
            None => f.write_str(tr!(
                "хост не разрешил подключение",
                "the host did not allow the connection"
            )),
        }
    }
}

impl std::error::Error for NotAllowed {}

#[derive(Debug, Clone)]
pub struct GuestConfig {
    pub host: EndpointAddr,
    /// The guest's game nickname: the host sees it in the "let in?" question and the guest list.
    pub nickname: Option<String>,
    /// The guest's rights token (Phase Pro), if any: sent to the host in the hello.
    pub entitlement: Option<String>,
    /// Local port for Minecraft. `None` means any free one.
    pub local_port: Option<u16>,
    /// Which voice mod UDP ports to open.
    pub voice: VoicePorts,
    /// How often to log the ping.
    pub status_every: Duration,
}

impl GuestConfig {
    fn intro(&self) -> Intro<'_> {
        Intro {
            nickname: self.nickname.as_deref(),
            entitlement: self.entitlement.as_deref(),
        }
    }
}

/// How the guest introduces itself to the host.
#[derive(Debug, Clone, Copy, Default)]
pub struct Intro<'a> {
    /// Game nickname (the host shows it next to the ID).
    pub nickname: Option<&'a str>,
    /// Rights token (Phase Pro).
    pub entitlement: Option<&'a str>,
}

/// A running guest. **The guest runs while this object is alive:** when it is dropped (or
/// when [`GuestHandle::wait`] is no longer awaited) the background task is aborted, the local
/// port and voice ports are closed, and reconnects stop.
pub struct GuestHandle {
    /// Address for Minecraft: "Direct Connection" → this address.
    pub local_addr: SocketAddr,
    pub host_info: HostInfo,
    /// `true` while the host is connected (for the LAN announcement and the interface).
    pub online: watch::Receiver<bool>,
    /// Open voice ports.
    pub voice_bound: Vec<BoundVoicePort>,
    /// Voice ports that could not be opened, and why.
    pub voice_skipped: Vec<(u16, String)>,
    voice_stats: Arc<VoiceStats>,
    connection: watch::Receiver<Option<Connection>>,
    task: AbortOnDropHandle<Result<()>>,
}

impl GuestHandle {
    /// Voice packet counters (live: can be read while running).
    pub fn voice_stats(&self) -> Arc<VoiceStats> {
        self.voice_stats.clone()
    }

    /// Current connection to the host (`None` while reconnecting). For monitoring
    /// paths and ping (`Connection::paths`).
    pub fn connection(&self) -> Option<Connection> {
        self.connection.borrow().clone()
    }

    /// The connection to the host as a channel: the value changes on a drop (`None`) and after
    /// reconnecting. For tasks that outlive one connection (mod download resume).
    pub fn watch_connection(&self) -> watch::Receiver<Option<Connection>> {
        self.connection.clone()
    }

    /// Runs until an unrecoverable error. If it is no longer awaited (the future
    /// is dropped), the guest stops.
    pub async fn wait(self) -> Result<()> {
        self.task.await.context(tr!(
            "задача гостя аварийно завершилась",
            "the guest task crashed"
        ))?
    }
}

/// Connects to the host, gets the world description and opens a local port
/// on `127.0.0.1`. Everything after that runs in the background: see [`GuestHandle::wait`].
pub async fn start(
    endpoint: Endpoint,
    cfg: GuestConfig,
    telemetry: Telemetry,
) -> Result<GuestHandle> {
    let (conn, host_info) = connect_with(&endpoint, &cfg.host, &cfg.intro()).await?;
    if host_info.game_port.is_none() {
        bail!(tr!(
            "хост запустил программу без --port: проброс игры выключен",
            "the host started the program without --port: game forwarding is off"
        ));
    }
    let listener = bind_local(cfg.local_port).await?;
    let local_addr = listener.local_addr()?;

    let label = format!(
        "{} {}",
        tr!("хост", "host"),
        wire::peer_label(&cfg.host.id, host_info.nickname.as_deref())
    );
    metrics::spawn_monitor(
        conn.clone(),
        label.clone(),
        cfg.status_every,
        telemetry.clone(),
    );
    let (tx, rx) = watch::channel(Some(conn.clone()));
    let (online_tx, online_rx) = watch::channel(true);

    let voice = voice::start_guest(&host_info.udp_ports, &cfg.voice, rx.clone());
    let voice_bound = voice.bound.clone();
    let voice_skipped = voice.skipped.clone();
    let voice_stats = voice.stats.clone();
    let connection = rx.clone();

    let task = AbortOnDropHandle::new(tokio::spawn(async move {
        let _voice = voice; // voice runs as long as the guest does
        tokio::select! {
            res = reconnect_loop(endpoint, cfg, conn, tx, online_tx, telemetry, label) => res,
            res = accept_loop(listener, rx) => res,
        }
    }));
    Ok(GuestHandle {
        local_addr,
        host_info,
        online: online_rx,
        voice_bound,
        voice_skipped,
        voice_stats,
        connection,
        task,
    })
}

/// One connection to the host + control message exchange. `nickname` is the game nickname
/// of this device (the host shows it next to the ID).
pub async fn connect_once(
    endpoint: &Endpoint,
    host: &EndpointAddr,
    nickname: Option<&str>,
) -> Result<(Connection, HostInfo)> {
    let intro = Intro {
        nickname,
        entitlement: None,
    };
    connect_with(endpoint, host, &intro).await
}

/// Like [`connect_once`], but the guest introduces itself fully: nickname and rights token.
pub async fn connect_with(
    endpoint: &Endpoint,
    host: &EndpointAddr,
    intro: &Intro<'_>,
) -> Result<(Connection, HostInfo)> {
    let conn = match endpoint.connect(host.clone(), wire::ALPN).await {
        Ok(conn) => conn,
        Err(err) => {
            // A common cause with our own relay: the relay refused (password) or is unreachable.
            let hint = crate::endpoint::relay_problem(endpoint)
                .map(|why| format!(" ({why})"))
                .unwrap_or_default();
            return Err(anyhow::Error::new(err).context(tr!(
                format!("не удалось подключиться к хосту{hint}"),
                format!("could not connect to the host{hint}")
            )));
        }
    };
    match control_handshake(&conn, intro).await {
        Ok(info) => Ok((conn, info)),
        Err(err) => Err(explain_close(&conn).unwrap_or(err)),
    }
}

async fn control_handshake(conn: &Connection, intro: &Intro<'_>) -> Result<HostInfo> {
    let (mut send, mut recv) = conn.open_bi().await.context(tr!(
        "не удалось открыть служебный поток",
        "could not open the control stream"
    ))?;
    wire::write_kind(&mut send, StreamKind::Control).await?;
    let hello = Hello::current(intro.nickname).with_entitlement(intro.entitlement);
    wire::write_msg(&mut send, &hello).await?;
    let _ = send.finish();
    let mut info: HostInfo = tokio::time::timeout(HANDSHAKE_TIMEOUT, wire::read_msg(&mut recv))
        .await
        .context(tr!("хост не ответил", "the host did not respond"))??;
    // Text from the host is shown in the console and the interface, so sanitize it.
    info.motd = wire::sanitize_text(&info.motd, wire::MOTD_MAX_CHARS);
    info.app_version = wire::sanitize_text(&info.app_version, 32);
    info.game_version = info
        .game_version
        .map(|v| wire::sanitize_text(&v, 64))
        .filter(|v| !v.is_empty());
    info.nickname = info.nickname.as_deref().and_then(wire::sanitize_nickname);
    // The host cannot make the guest open many ports.
    info.udp_ports.sort_unstable();
    info.udp_ports.dedup();
    info.udp_ports.truncate(MAX_VOICE_PORTS);
    if info.protocol_version != wire::PROTOCOL_VERSION {
        let (theirs, ours) = (info.protocol_version, wire::PROTOCOL_VERSION);
        bail!(tr!(
            format!(
                "у хоста другая версия протокола ({theirs}, у вас {ours}). Нужна одинаковая версия программы."
            ),
            format!(
                "the host uses a different protocol version ({theirs}, yours is {ours}). Both of you need the same version of the program."
            )
        ));
    }
    Ok(info)
}

/// How long to wait for the host's answer to the styling question.
const LOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// Asks the host how this device looks in its world and (if `request.glow`
/// is set) asks to turn the glow **on itself** on or off. The stream may be opened
/// only if the host announced it in [`wire::RoomInfo::look`]: an older host does not know
/// this stream.
pub async fn look(conn: &Connection, request: wire::LookRequest) -> Result<wire::LookState> {
    let exchange = async {
        let (mut send, mut recv) = conn.open_bi().await.context(tr!(
            "не удалось открыть поток оформления",
            "could not open the room style stream"
        ))?;
        wire::write_kind(&mut send, StreamKind::Look).await?;
        wire::write_msg(&mut send, &request).await?;
        let _ = send.finish();
        let state: wire::LookState = wire::read_msg(&mut recv).await?;
        Ok(state.sanitized())
    };
    tokio::time::timeout(LOOK_TIMEOUT, exchange)
        .await
        .context(tr!(
            "хост не ответил на вопрос об оформлении",
            "the host did not answer the room style request"
        ))?
}

/// A readable explanation if the host closed the connection.
fn explain_close(conn: &Connection) -> Option<anyhow::Error> {
    match conn.close_reason()? {
        ConnectionError::ApplicationClosed(close)
            if close.error_code == VarInt::from_u32(code::NOT_ALLOWED) =>
        {
            Some(anyhow::Error::new(NotAllowed {
                reason: RejectReason::from_bytes(&close.reason),
            }))
        }
        // The host sends the close reason, so sanitize the text before showing it.
        other => {
            let why = wire::sanitize_text(&other.to_string(), 200);
            Some(anyhow!(tr!(
                format!("хост закрыл соединение: {why}"),
                format!("the host closed the connection: {why}")
            )))
        }
    }
}

/// Local port for Minecraft. Only `127.0.0.1`: other devices on the network cannot
/// connect to it.
///
/// By default any free port Windows picks. The host's port number is
/// deliberately not used:
/// - someone else's ticket could make us take the port of a local service on this PC;
/// - if the host and the guest are on one PC and the world is not open yet, the host would hit our own port
///   and connections would loop.
async fn bind_local(explicit: Option<u16>) -> Result<TcpListener> {
    let port = explicit.unwrap_or(0);
    TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .with_context(|| {
            tr!(
                format!("не удалось открыть порт 127.0.0.1:{port} (занят?)"),
                format!("could not open port 127.0.0.1:{port} (in use?)")
            )
        })
}

async fn accept_loop(listener: TcpListener, rx: watch::Receiver<Option<Connection>>) -> Result<()> {
    loop {
        // An error on one incoming connection (for example, the client dropped it before accept)
        // must not stop the guest or disconnect the other players.
        let (tcp, from) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                warn!("ошибка приёма локального подключения: {err}");
                // Pause so we do not spin idle when resources run short.
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if !from.ip().is_loopback() {
            // Should not happen: the port is open only on 127.0.0.1. A check just in case.
            warn!("отклонено подключение не с этого компьютера: {from}");
            continue;
        }
        let mut rx = rx.clone();
        tokio::spawn(async move {
            let conn = match tokio::time::timeout(WAIT_FOR_HOST, rx.wait_for(Option::is_some)).await
            {
                Ok(Ok(current)) => current.clone(),
                _ => None,
            };
            let Some(conn) = conn else {
                warn!("нет связи с хостом — подключение игры отклонено");
                return;
            };
            info!("игровое подключение с {from}");
            match forward_one(conn, tcp).await {
                Ok(()) => info!("игровое подключение закрыто"),
                Err(err) => info!("игровое подключение закрыто: {err:#}"),
            }
        });
    }
}

async fn forward_one(conn: Connection, tcp: TcpStream) -> Result<()> {
    tcp.set_nodelay(true)?;
    let (mut send, recv) = conn.open_bi().await.context(tr!(
        "не удалось открыть поток к хосту",
        "could not open a stream to the host"
    ))?;
    wire::write_kind(&mut send, StreamKind::Tcp).await?;
    forward::pipe(tcp, send, recv).await
}

async fn reconnect_loop(
    endpoint: Endpoint,
    cfg: GuestConfig,
    mut conn: Connection,
    tx: watch::Sender<Option<Connection>>,
    online: watch::Sender<bool>,
    telemetry: Telemetry,
    label: String,
) -> Result<()> {
    loop {
        let reason = conn.closed().await;
        let _ = tx.send(None);
        let _ = online.send(false);
        warn!("связь с хостом потеряна ({reason}). Переподключаюсь…");
        telemetry.emit(TunnelEvent::HostLost {
            reason: reason.to_string(),
        });
        let mut delay = Duration::from_secs(1);
        conn = loop {
            match connect_with(&endpoint, &cfg.host, &cfg.intro()).await {
                Ok((conn, _info)) => break conn,
                Err(err) if err.is::<NotAllowed>() => return Err(err),
                Err(err) => {
                    warn!(
                        "не удалось переподключиться: {err:#}. Следующая попытка через {delay:?}"
                    );
                    telemetry.emit(TunnelEvent::ReconnectFailed {
                        error: format!("{err:#}"),
                        retry_in_ms: delay.as_millis().try_into().unwrap_or(u64::MAX),
                    });
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(MAX_BACKOFF);
                }
            }
        };
        info!("связь с хостом восстановлена");
        telemetry.emit(TunnelEvent::HostRestored);
        metrics::spawn_monitor(
            conn.clone(),
            label.clone(),
            cfg.status_every,
            telemetry.clone(),
        );
        let _ = tx.send(Some(conn.clone()));
        let _ = online.send(true);
    }
}
