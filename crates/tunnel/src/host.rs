//! Host: accepts allowed guests and forwards their connections to its game.

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use iroh::{
    Endpoint, EndpointId,
    endpoint::{Connection, RecvStream, SendStream, VarInt},
};
use tokio::{io::AsyncWriteExt, net::TcpStream};
use tracing::{debug, info, warn};

use crate::{
    access::AccessList,
    echo,
    events::TunnelEvent,
    forward,
    metrics::{self, Telemetry},
    mods::{self, ModShare},
    voice::{self, VoiceStats},
    wire::{self, Hello, HostInfo, StreamKind, code},
};

/// How long to wait for the first byte (stream type) from the guest.
const KIND_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the connection to the game on `127.0.0.1`.
const GAME_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The game port guest game connections go to. The host program can change it
/// without closing the room (the host left the world and opened it to LAN again, and the game took
/// a new port): connections already running stay on the old port, the next ones go to the
/// new one. Clones share the same port.
#[derive(Debug, Clone, Default)]
pub struct GameTarget(Arc<AtomicU16>);

impl GameTarget {
    /// `None` means game forwarding is off.
    pub fn new(port: Option<u16>) -> Self {
        Self(Arc::new(AtomicU16::new(port.unwrap_or(0))))
    }

    /// Current game port (`None` means forwarding is off). Port 0 is never a target.
    pub fn get(&self) -> Option<u16> {
        Some(self.0.load(Ordering::Acquire)).filter(|port| *port != 0)
    }

    /// New game connections will go to this port.
    pub fn set(&self, port: u16) {
        self.0.store(port, Ordering::Release);
    }
}

impl From<Option<u16>> for GameTarget {
    fn from(port: Option<u16>) -> Self {
        Self::new(port)
    }
}

impl From<u16> for GameTarget {
    fn from(port: u16) -> Self {
        Self::new(Some(port))
    }
}

#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Game port on this PC (a LAN world or your own server). Connections go **only** to
    /// `127.0.0.1:<port>`. Empty means game forwarding is off (echo diagnostics remain).
    /// The port can change while running (see [`GameTarget`]).
    pub game_port: GameTarget,
    pub motd: String,
    /// The host's game nickname: the guest sees whom it connected to.
    pub nickname: Option<String>,
    /// Voice mod UDP ports on this PC. Guest packets go **only** to
    /// `127.0.0.1:<port from this list>`. Empty means voice is not forwarded.
    pub udp_ports: Vec<u16>,
    /// Game version (from the status request): the guest sees which version is needed.
    pub game_version: Option<String>,
    /// Check during play that the voice ports are held by the game process (see
    /// [`voice::PortGuard`]). `false` means the host gave the ports itself (--udp) and is responsible for them.
    pub check_voice_owner: bool,
    /// How often to log each guest's ping.
    pub status_every: Duration,
    /// Mods the host shares with guests (an empty share means no mods are required). The set
    /// can change while the room is open.
    pub mods: ModShare,
    /// Room: slots and what the host program knows about it (plan, guest rights).
    pub room: RoomOptions,
}

/// Room settings for the network core.
#[derive(Debug, Clone, Default)]
pub struct RoomOptions {
    /// How many players the room holds including the host (0 means no limit). While
    /// running, the limit changes through [`AccessList::gate`].
    pub max_players: u32,
    /// The host program: checks guest rights and says what kind of room this is. Without it
    /// the room is like in the version without plans.
    pub service: Option<Arc<dyn RoomService>>,
}

/// What the network core asks the host program about the room. Called right in the core's
/// task: answers must be fast and non-blocking.
pub trait RoomService: Send + Sync + std::fmt::Debug + 'static {
    /// A guest introduced itself. `entitlement` is its rights token as sent: the implementation checks
    /// the signature, expiry and device binding.
    fn hello(&self, peer: &EndpointId, entitlement: Option<&str>);

    /// What to tell this guest about the room.
    fn info(&self, peer: &EndpointId) -> wire::RoomInfo;

    /// Start of the guest's game stream (the first bytes, before encryption): which nickname the game
    /// joins the world under. The network core knows nothing about the game; the host program parses
    /// the format. The bytes have already gone to the game unchanged: this is read-only.
    fn sniff_game(&self, _head: &[u8]) -> GameSniff {
        GameSniff::Skip
    }

    /// Whether to let the guest's game into the world under this nickname. Asked when
    /// [`sniff_game`](Self::sniff_game) has learned the nickname, **before** the start of the stream goes to
    /// the game: a rejection means the host's game never sees this connection at all.
    fn admit_game(&self, _peer: &EndpointId, _name: &str) -> GameAdmit {
        GameAdmit::Allow
    }

    /// The guest asks about its in-game styling and can toggle its glow.
    fn look(&self, _peer: &EndpointId, _request: wire::LookRequest) -> wire::LookState {
        wire::LookState::default()
    }
}

/// What the start of the game stream shows ([`RoomService::sniff_game`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameSniff {
    /// Not enough bytes yet: ask again when more arrive.
    NeedMore,
    /// This is not a game login (a status request, another protocol): stop looking.
    Skip,
    /// The game joins the world under this nickname.
    Login { name: String, uuid: Option<String> },
}

/// Decision on the guest's game joining the world ([`RoomService::admit_game`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameAdmit {
    Allow,
    /// Do not let in. `reply` goes to the guest's game as is (the host program knows the format:
    /// for Minecraft, a disconnect packet with an on-screen reason), `reason` goes to the log.
    Refuse {
        reason: String,
        reply: Vec<u8>,
    },
}

/// How reading the start of the game stream ended.
#[derive(Debug, PartialEq, Eq)]
enum Sniffed {
    /// Nickname unknown: the stream continues without styling.
    Unknown,
    Login {
        name: String,
        uuid: Option<String>,
    },
    Refused {
        name: String,
        reason: String,
    },
}

/// How many bytes of the start of the game stream are shown to the host program.
pub const SNIFF_MAX: usize = 1024;
/// How long to wait for those bytes. The game sends them right after connecting.
const SNIFF_TIMEOUT: Duration = Duration::from_secs(5);

/// Accepts incoming connections until the endpoint is closed.
pub async fn serve(
    endpoint: Endpoint,
    access: AccessList,
    cfg: HostConfig,
    telemetry: Telemetry,
) -> Result<()> {
    access.gate().set_limit(cfg.room.max_players);
    let cfg = Arc::new(cfg);
    while let Some(incoming) = endpoint.accept().await {
        let (access, cfg, telemetry) = (access.clone(), cfg.clone(), telemetry.clone());
        tokio::spawn(async move {
            match incoming.await {
                Ok(conn) => handle_connection(conn, access, cfg, telemetry).await,
                // Connections rejected by the admission check end up here too.
                Err(err) => debug!("входящее подключение не установлено: {err:#}"),
            }
        });
    }
    Ok(())
}

/// One connected guest: data shared by all its streams.
struct Guest {
    id: EndpointId,
    /// The nickname the guest is shown under now (changes when the guest introduces itself).
    nickname: Mutex<Option<String>>,
    access: AccessList,
    cfg: Arc<HostConfig>,
    telemetry: Telemetry,
}

impl Guest {
    /// "Nick (#1603817a)" or "#1603817a", for the log.
    fn label(&self) -> String {
        wire::peer_label(
            &self.id,
            self.nickname.lock().expect("nickname lock").as_deref(),
        )
    }

    /// A guest introduced itself. The new nickname is saved in the allow list and sent to the
    /// interface; the same nickname changes nothing.
    fn introduce(&self, hello: &Hello) {
        let Some(nickname) = hello.nickname() else {
            return;
        };
        {
            let mut current = self.nickname.lock().expect("nickname lock");
            if current.as_deref() == Some(nickname.as_str()) {
                return;
            }
            *current = Some(nickname.clone());
        }
        if let Err(err) = self.access.remember_nickname(&self.id, &nickname) {
            warn!("не удалось сохранить ник в списке разрешённых: {err:#}");
        }
        info!("игрок {} представился", self.label());
        self.telemetry.emit(TunnelEvent::PeerNickname {
            peer: self.id.to_string(),
            nickname,
        });
    }
}

async fn handle_connection(
    conn: Connection,
    access: AccessList,
    cfg: Arc<HostConfig>,
    telemetry: Telemetry,
) {
    let peer = conn.remote_id();
    // The first stream may have been read by AccessHook (the nickname for the "let in?" question).
    let early = access.take_early(conn.stable_id());
    // Second line of defense: the check already passed in AccessHook, but repeat it here.
    if !access.is_allowed(&peer) {
        warn!(
            "подключение {} без разрешения — закрыто",
            wire::short_id(&peer)
        );
        conn.close(VarInt::from_u32(code::NOT_ALLOWED), b"not allowed");
        return;
    }
    // Room slot: the admission check already looked at the limit, but two guests could pass
    // it at once, and only one gets the last slot.
    let gate = access.gate().clone();
    if !gate.enter(peer) {
        let max = gate.limit();
        info!(
            "подключение {} закрыто: комната заполнена ({max} из {max})",
            wire::short_id(&peer)
        );
        conn.close(
            VarInt::from_u32(code::NOT_ALLOWED),
            &wire::RejectReason::RoomFull { max }.to_bytes(),
        );
        return;
    }
    // Nickname at connection time: from the hello (a new device) or the one this friend
    // was remembered under last time. A fresh nickname arrives in the hello a bit later.
    let nickname = early
        .as_ref()
        .and_then(|e| e.hello.as_ref())
        .and_then(Hello::nickname)
        .or_else(|| access.nickname(&peer));
    let guest = Arc::new(Guest {
        id: peer,
        nickname: Mutex::new(nickname.clone()),
        access,
        cfg: cfg.clone(),
        telemetry: telemetry.clone(),
    });
    info!("игрок {} подключился", guest.label());
    telemetry.emit(TunnelEvent::PeerConnected {
        peer: peer.to_string(),
        nickname,
    });
    let label = format!("игрок {}", guest.label());
    let monitor = metrics::spawn_monitor(
        conn.clone(),
        label.clone(),
        cfg.status_every,
        telemetry.clone(),
    );
    let voice_stats = Arc::new(VoiceStats::default());
    let voice = (!cfg.udp_ports.is_empty()).then(|| {
        tokio::spawn(voice::serve_host(
            conn.clone(),
            cfg.udp_ports.clone().into(),
            Arc::new(if cfg.check_voice_owner {
                voice::PortGuard::new(cfg.game_port.clone())
            } else {
                voice::PortGuard::trust_all()
            }),
            voice_stats.clone(),
        ))
    });

    if let Some(early) = early {
        let guest = guest.clone();
        tokio::spawn(async move {
            let stream = serve_stream(early.kind, early.hello, early.send, early.recv, &guest);
            if let Err(err) = stream.await {
                debug!("поток игрока {} завершён: {err:#}", guest.label());
            }
        });
    }
    let reason = loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(streams) => streams,
            Err(err) => {
                info!("игрок {} отключился: {err}", guest.label());
                break err.to_string();
            }
        };
        let guest = guest.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_stream(send, recv, &guest).await {
                debug!("поток игрока {} завершён: {err:#}", guest.label());
            }
        });
    };
    if let Some(voice) = voice {
        let _ = voice.await; // ends together with the connection
        voice::log_summary(&label, &voice_stats);
        let counts = voice_stats.snapshot();
        if counts.is_used() {
            telemetry.emit(TunnelEvent::VoiceSummary {
                peer: peer.to_string(),
                counts,
            });
        }
    }
    let _ = monitor.await;
    gate.leave(&peer);
    telemetry.emit(TunnelEvent::PeerDisconnected {
        peer: peer.to_string(),
        reason,
    });
}

/// Reads the start of the guest's game stream and shows it to the host program: this is how it
/// learns the guest's game nickname and decides whether to let it in ([`RoomService::admit_game`]).
/// What was read is held here until there is a decision, then goes to the game **unchanged**
/// in one piece; on a rejection the host's game gets not a single byte, and the guest's game gets the
/// host program's reply. Reading ends as soon as there is an answer, [`SNIFF_MAX`] bytes have
/// accumulated, [`SNIFF_TIMEOUT`] has passed, the stream ended or the game spoke
/// first (then it is not Minecraft: a game server always waits for the client). After that the stream
/// goes through the normal pump: if the start could not be parsed, there is simply no styling.
///
/// The only error is a write to the game: the connection to it dropped.
async fn sniff_game(
    service: &dyn RoomService,
    peer: &EndpointId,
    send: &mut SendStream,
    recv: &mut RecvStream,
    tcp: &mut TcpStream,
) -> Result<Sniffed> {
    let deadline = tokio::time::Instant::now() + SNIFF_TIMEOUT;
    let mut head = Vec::with_capacity(256);
    let mut buf = [0u8; SNIFF_MAX];
    let sniffed = loop {
        let room = SNIFF_MAX - head.len();
        let read = tokio::select! {
            read = tokio::time::timeout_at(deadline, recv.read(&mut buf[..room])) => read,
            // The pump will pick up data from the game itself: all that matters here is that there is some.
            _ = tcp.readable() => break Sniffed::Unknown,
        };
        let len = match read {
            Ok(Ok(Some(len))) => len,
            // Time ran out, the stream ended or dropped: the pump will deal with it.
            _ => break Sniffed::Unknown,
        };
        head.extend_from_slice(&buf[..len]);
        match service.sniff_game(&head) {
            GameSniff::Login { name, uuid } => match service.admit_game(peer, &name) {
                GameAdmit::Allow => break Sniffed::Login { name, uuid },
                GameAdmit::Refuse { reason, reply } => {
                    // The guest's game gets the on-screen reason; nothing goes to the host's game.
                    let _ = send.write_all(&reply).await;
                    let _ = send.finish();
                    return Ok(Sniffed::Refused { name, reason });
                }
            },
            GameSniff::Skip => break Sniffed::Unknown,
            GameSniff::NeedMore if head.len() >= SNIFF_MAX => break Sniffed::Unknown,
            GameSniff::NeedMore => {}
        }
    };
    tcp.write_all(&head).await.context(phase_lang::tr!(
        "ошибка записи в локальное подключение",
        "error writing to the local connection"
    ))?;
    Ok(sniffed)
}

async fn handle_stream(send: SendStream, mut recv: RecvStream, guest: &Guest) -> Result<()> {
    let kind = tokio::time::timeout(KIND_TIMEOUT, wire::read_kind(&mut recv))
        .await
        .context(phase_lang::tr!(
            "гость не прислал тип потока",
            "the guest did not send a stream type"
        ))??;
    serve_stream(kind, None, send, recv, guest).await
}

/// Serves a stream of a known type. `hello` is the hello, if the admission check already read it
/// (see [`crate::access::AccessHook`]).
async fn serve_stream(
    kind: StreamKind,
    hello: Option<Hello>,
    mut send: SendStream,
    mut recv: RecvStream,
    guest: &Guest,
) -> Result<()> {
    let cfg = &guest.cfg;
    match kind {
        StreamKind::Control => {
            let hello: Hello = match hello {
                Some(hello) => hello,
                None => wire::read_msg(&mut recv).await?,
            };
            if hello.protocol_version != wire::PROTOCOL_VERSION {
                warn!(
                    "у гостя другая версия протокола ({}, у нас {}); гостю нужно обновить программу",
                    hello.protocol_version,
                    wire::PROTOCOL_VERSION
                );
            }
            guest.introduce(&hello);
            let room = cfg.room.service.as_ref().map(|service| {
                service.hello(&guest.id, hello.entitlement());
                service.info(&guest.id)
            });
            let info = HostInfo {
                protocol_version: wire::PROTOCOL_VERSION,
                app_version: wire::APP_VERSION.to_string(),
                game_port: cfg.game_port.get(),
                motd: cfg.motd.clone(),
                udp_ports: cfg.udp_ports.clone(),
                game_version: cfg.game_version.clone(),
                nickname: cfg.nickname.as_deref().and_then(wire::sanitize_nickname),
                mods: Some(cfg.mods.offer()),
                room,
            };
            wire::write_msg(&mut send, &info).await?;
            let _ = send.finish();
            Ok(())
        }
        StreamKind::Tcp => {
            // The port is read on every connection: the host may have reopened the world on another one.
            let Some(port) = cfg.game_port.get() else {
                let _ = send.reset(VarInt::from_u32(code::GAME_UNREACHABLE));
                bail!(phase_lang::tr!(
                    "проброс игры выключен: хост запущен без --port",
                    "game forwarding is off: the host was started without --port"
                ));
            };
            // Only the host sets the target. An address from the guest is never accepted.
            let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            let tcp = match tokio::time::timeout(GAME_CONNECT_TIMEOUT, TcpStream::connect(target))
                .await
            {
                Ok(Ok(tcp)) => tcp,
                Ok(Err(err)) => {
                    let _ = send.reset(VarInt::from_u32(code::GAME_UNREACHABLE));
                    warn!(
                        "не удалось подключиться к игре на {target}: {err}. Мир открыт для сети? Порт верный?"
                    );
                    guest.telemetry.emit(TunnelEvent::GameUnreachable {
                        port,
                        error: err.to_string(),
                    });
                    return Ok(());
                }
                Err(_) => {
                    let _ = send.reset(VarInt::from_u32(code::GAME_UNREACHABLE));
                    warn!("игра на {target} не ответила за {GAME_CONNECT_TIMEOUT:?}");
                    guest.telemetry.emit(TunnelEvent::GameUnreachable {
                        port,
                        error: phase_lang::tr!(
                            format!("нет ответа за {} с", GAME_CONNECT_TIMEOUT.as_secs()),
                            format!("no response within {} s", GAME_CONNECT_TIMEOUT.as_secs())
                        ),
                    });
                    return Ok(());
                }
            };
            tcp.set_nodelay(true)?;
            info!("игровое подключение → {target}");
            let mut tcp = tcp;
            if let Some(service) = &cfg.room.service {
                match sniff_game(service.as_ref(), &guest.id, &mut send, &mut recv, &mut tcp).await
                {
                    Ok(Sniffed::Login { name, uuid }) => {
                        info!("игрок {} входит в мир как {name}", guest.label());
                        guest.telemetry.emit(TunnelEvent::PeerGameName {
                            peer: guest.id.to_string(),
                            name,
                            uuid,
                        });
                    }
                    Ok(Sniffed::Refused { name, reason }) => {
                        warn!(
                            "игроку {} отказано во входе как {name}: {reason}",
                            guest.label()
                        );
                        guest.telemetry.emit(TunnelEvent::GameRefused {
                            peer: guest.id.to_string(),
                            name,
                            reason,
                        });
                        let _ = recv.stop(VarInt::from_u32(code::TCP_ERROR));
                        info!("игровое подключение закрыто");
                        return Ok(());
                    }
                    Ok(Sniffed::Unknown) => {}
                    Err(err) => {
                        let _ = recv.stop(VarInt::from_u32(code::TCP_ERROR));
                        info!("игровое подключение закрыто");
                        return Err(err);
                    }
                }
            }
            let result = forward::pipe(tcp, send, recv).await;
            info!("игровое подключение закрыто");
            result
        }
        StreamKind::Look => {
            let request: wire::LookRequest = wire::read_msg(&mut recv).await?;
            let state = match &cfg.room.service {
                Some(service) => service.look(&guest.id, request),
                None => wire::LookState::default(),
            };
            wire::write_msg(&mut send, &state).await?;
            let _ = send.finish();
            Ok(())
        }
        StreamKind::Echo => echo::serve(send, recv).await,
        StreamKind::Mods => {
            // The guest names a mod only by a hash from the list: no path is accepted from it.
            match mods::serve(&mut send, &mut recv, &cfg.mods).await? {
                mods::Served::Manifest { files } => {
                    debug!("игрок {} получил список модов ({files})", guest.label());
                }
                mods::Served::File { file_name, sent } => {
                    info!("мод {file_name} отправлен игроку {}", guest.label());
                    guest.telemetry.emit(TunnelEvent::ModSent {
                        peer: guest.id.to_string(),
                        file_name,
                        size: sent,
                    });
                }
                mods::Served::Refused(reason) => {
                    debug!("игроку {} отказано в моде: {reason}", guest.label());
                }
            }
            Ok(())
        }
    }
}
