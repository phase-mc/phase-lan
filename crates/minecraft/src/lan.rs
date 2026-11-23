//! Announcements of Minecraft Java Edition LAN worlds.
//!
//! When a player clicks "Open to LAN", the game sends the text
//! `[MOTD]name[/MOTD][AD]port[/AD]` to the multicast address `224.0.2.60:4445` every 1.5 s. Another
//! player's game shows the world in the "Multiplayer → LAN" list and connects to the
//! **sender's IP address** and the port from `[AD]`. The mechanism has not changed since version 1.3.1.
//!
//! - Guest: [`run_announcer`] sends such an announcement to their own game from the address
//!   `127.0.0.1`, giving the port of the local "twin" of the server. The game connects to
//!   `127.0.0.1:<port>`, that is, to our program.
//! - Host: [`detect_local_world`] listens for announcements on its own PC and learns the port
//!   of the open world.

use phase_lang::tr;
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4},
    str::FromStr,
    time::Duration,
};

use anyhow::{Context, Result};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tokio::{net::UdpSocket, sync::watch};
use tracing::{debug, warn};

use crate::status::{self, ServerStatus};

/// Multicast address of Minecraft LAN announcements.
pub const LAN_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 2, 60);
/// Port of Minecraft LAN announcements.
pub const LAN_PORT: u16 = 4445;
/// How often the game (and we) send an announcement.
pub const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1500);
/// Tag in the world name: shows that the world goes through Phase, and our world is not confused
/// with a real LAN world on the same network.
pub const MOTD_TAG: &str = " [Phase]";
const MAX_MOTD_CHARS: usize = 64;
/// How long to wait before checking again a port that did not answer the status request.
const RETRY_AFTER: Duration = Duration::from_secs(3);

/// LAN world announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    pub motd: String,
    pub port: u16,
}

/// Announcement text. Brackets in the name are replaced with round ones: otherwise a name like
/// `[/MOTD]` would break the game's parsing.
pub fn format_announcement(motd: &str, port: u16) -> String {
    let clean: String = motd
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| match c {
            '[' => '(',
            ']' => ')',
            c => c,
        })
        .take(MAX_MOTD_CHARS)
        .collect();
    format!("[MOTD]{clean}{MOTD_TAG}[/MOTD][AD]{port}[/AD]")
}

/// Parses an announcement (as the game does). `None` means it is not an announcement or has no port.
pub fn parse_announcement(text: &str) -> Option<Announcement> {
    let motd = between(text, "[MOTD]", "[/MOTD]")?;
    let port = between(text, "[AD]", "[/AD]")?.trim().parse().ok()?;
    Some(Announcement {
        motd: motd.to_string(),
        port,
    })
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = text.find(start)? + start.len();
    let to = text[from..].find(end)? + from;
    Some(&text[from..to])
}

/// How to send the announcement to our own game. Which method works more reliably will be shown by
/// test T2; the fallback is "Direct Connection" to the address the program prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnnounceMode {
    /// Directly to `127.0.0.1:4445` (the game listens on this port on all addresses).
    #[default]
    Unicast,
    /// Multicast to `224.0.2.60:4445` through the `127.0.0.1` adapter.
    Multicast,
    /// Both methods at once.
    Both,
    /// Do not announce (only "Direct Connection").
    Off,
}

impl AnnounceMode {
    fn unicast(self) -> bool {
        matches!(self, Self::Unicast | Self::Both)
    }

    fn multicast(self) -> bool {
        matches!(self, Self::Multicast | Self::Both)
    }
}

impl FromStr for AnnounceMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "unicast" => Ok(Self::Unicast),
            "multicast" => Ok(Self::Multicast),
            "both" => Ok(Self::Both),
            "off" => Ok(Self::Off),
            _ => Err(tr!(
                format!("неизвестный режим «{s}»: используйте unicast, multicast, both или off"),
                format!("unknown mode \"{s}\": use unicast, multicast, both or off")
            )),
        }
    }
}

/// Where to send announcements. In normal operation, the game's addresses; in tests, a custom port.
#[derive(Debug, Clone, Copy)]
pub struct AnnounceTarget {
    pub unicast: SocketAddr,
    pub multicast: SocketAddrV4,
}

impl Default for AnnounceTarget {
    fn default() -> Self {
        Self {
            unicast: SocketAddr::from((Ipv4Addr::LOCALHOST, LAN_PORT)),
            multicast: SocketAddrV4::new(LAN_GROUP, LAN_PORT),
        }
    }
}

/// Announces the world to our own game while `online == true` (there is a connection to the host).
///
/// Without a connection no announcement is sent: otherwise the world would hang in the list but not open.
/// Runs while the `online` sender is alive.
pub async fn run_announcer(
    mode: AnnounceMode,
    target: AnnounceTarget,
    announcement: Announcement,
    online: watch::Receiver<bool>,
) -> Result<()> {
    if mode == AnnounceMode::Off {
        return Ok(());
    }
    let payload = format_announcement(&announcement.motd, announcement.port).into_bytes();
    let unicast = if mode.unicast() {
        Some(
            UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .context(tr!(
                    "не удалось открыть сокет для объявлений",
                    "could not open a socket for announcements"
                ))?,
        )
    } else {
        None
    };
    let multicast = if mode.multicast() {
        Some(multicast_sender().context(tr!(
            "не удалось открыть сокет для групповой рассылки",
            "could not open a socket for multicast"
        ))?)
    } else {
        None
    };

    let mut tick = tokio::time::interval(ANNOUNCE_INTERVAL);
    loop {
        tick.tick().await;
        if online.has_changed().is_err() {
            return Ok(()); // the guest has finished
        }
        if !*online.borrow() {
            continue;
        }
        // Send errors are harmless: for example, the game is not running yet.
        if let Some(socket) = &unicast
            && let Err(err) = socket.send_to(&payload, target.unicast).await
        {
            debug!("объявление (unicast) не отправлено: {err}");
        }
        if let Some(socket) = &multicast
            && let Err(err) = socket.send_to(&payload, target.multicast).await
        {
            debug!("объявление (multicast) не отправлено: {err}");
        }
    }
}

/// A multicast socket that does not leave the PC: bound to `127.0.0.1`.
fn multicast_sender() -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.bind(&SockAddr::from(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)))?;
    socket.set_multicast_if_v4(&Ipv4Addr::LOCALHOST)?;
    socket.set_multicast_loop_v4(true)?;
    socket.set_multicast_ttl_v4(1)?;
    socket.set_nonblocking(true)?;
    Ok(UdpSocket::from_std(socket.into())?)
}

/// A LAN world found on this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorld {
    pub announcement: Announcement,
    /// Reply to the status request: the port definitely hosts a Minecraft server.
    pub status: ServerStatus,
}

/// Looks for an open LAN world **on this PC**: listens for announcements for up to `wait`.
///
/// An announcement is accepted only if:
/// - it was sent from this same PC (not by a neighbor on the home network);
/// - it is not our own announcement (the [`MOTD_TAG`] tag);
/// - the port in the announcement answers as a **Minecraft server** (status request, see [`status`]).
///   This way the program will not expose some other service to friends, even if someone on this PC
///   forges an announcement.
pub async fn detect_local_world(wait: Duration) -> Result<Option<LocalWorld>> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // The game (the "Multiplayer" screen) also listens on 4445, so we share the port with it.
    socket.set_reuse_address(true)?;
    socket
        .bind(&SockAddr::from(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            LAN_PORT,
        )))
        .context(tr!(
            "не удалось открыть порт 4445 для поиска LAN-мира",
            "could not open port 4445 to look for a LAN World"
        ))?;
    socket
        .join_multicast_v4(&LAN_GROUP, &Ipv4Addr::UNSPECIFIED)
        .context(tr!(
            "не удалось подписаться на LAN-объявления",
            "could not subscribe to LAN announcements"
        ))?;
    socket.set_nonblocking(true)?;
    let announced = detect_on(UdpSocket::from_std(socket.into())?, wait);
    // Whichever comes first: an announcement or the game process's port.
    tokio::select! {
        found = announced => found,
        Some(found) = probe_game_ports(wait) => Ok(Some(found)),
    }
}

/// Fallback search for when announcements do not reach Phase: a VPN such as Radmin grabs
/// the game's multicast onto its own adapter, and the main network's 4445 is silent. Then we look at the TCP ports
/// the game process on this PC listens on and ask them for status. A foreign port cannot get in here:
/// only game processes, and only one that answered as a Minecraft server.
async fn probe_game_ports(wait: Duration) -> Option<LocalWorld> {
    let search = async {
        let mut retry_after: HashMap<u16, tokio::time::Instant> = HashMap::new();
        loop {
            let ports = tokio::task::spawn_blocking(game_listen_ports)
                .await
                .unwrap_or_default();
            for port in ports {
                if retry_after
                    .get(&port)
                    .is_some_and(|at| tokio::time::Instant::now() < *at)
                {
                    continue;
                }
                let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
                match status::query(addr, status::DEFAULT_TIMEOUT).await {
                    Ok(status) => {
                        let motd = status.motd.clone();
                        return LocalWorld {
                            announcement: Announcement { motd, port },
                            status,
                        };
                    }
                    Err(err) => {
                        debug!("порт {port} процесса игры не ответил как Minecraft: {err:#}");
                        retry_after.insert(port, tokio::time::Instant::now() + RETRY_AFTER);
                    }
                }
            }
            tokio::time::sleep(ANNOUNCE_INTERVAL).await;
        }
    };
    tokio::time::timeout(wait, search).await.ok()
}

/// TCP ports that game processes (`javaw.exe`, `java.exe`) listen on. The call blocks.
fn game_listen_ports() -> Vec<u16> {
    let mut ports: Vec<u16> = game_ports()
        .listening
        .into_iter()
        .map(|(port, _)| port)
        .collect();
    ports.dedup();
    ports
}

/// Snapshot of games on this PC: which game processes are running and which TCP ports they listen on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GamePorts {
    /// Running game processes (PIDs).
    pub games: Vec<u32>,
    /// Ports the game processes listen on: `(port, PID)`, ascending by port.
    pub listening: Vec<(u16, u32)>,
}

/// Game processes and their TCP ports. If the tables could not be read, the snapshot is empty. The call
/// blocks.
pub fn game_ports() -> GamePorts {
    let Ok(games) = crate::game_process::game_processes() else {
        return GamePorts::default();
    };
    let games: Vec<u32> = games.iter().map(|game| game.pid).collect();
    let Ok(listeners) = phase_netstat::tcp_listeners() else {
        return GamePorts {
            games,
            listening: Vec::new(),
        };
    };
    let mut listening: Vec<(u16, u32)> = listeners
        .into_iter()
        .filter(|listener| games.contains(&listener.pid))
        .map(|listener| (listener.port, listener.pid))
        .collect();
    listening.sort_unstable();
    listening.dedup();
    GamePorts { games, listening }
}

/// Processes (any) listening on a TCP port: empty means nobody listens on the port. An error means
/// the port table could not be read. The call blocks.
pub fn port_listeners(port: u16) -> std::io::Result<Vec<u32>> {
    Ok(phase_netstat::owners_of(
        &phase_netstat::tcp_listeners()?,
        port,
    ))
}

/// Parsing of announcements that arrived on `socket` (separate so that tests can listen
/// on `127.0.0.1` only, without a firewall prompt).
pub async fn detect_on(socket: UdpSocket, wait: Duration) -> Result<Option<LocalWorld>> {
    let search = async {
        let mut buf = [0u8; 2048];
        // Ports where the status request failed, and when they may be checked again: we do not
        // poll them with every announcement (every 1.5 s), but we do not strike them off for good either:
        // the world may simply not have had time to answer (just opened, autosave).
        let mut retry_after: HashMap<u16, tokio::time::Instant> = HashMap::new();
        loop {
            let (len, from) = match socket.recv_from(&mut buf).await {
                Ok(received) => received,
                Err(err) => {
                    debug!("ошибка приёма объявления: {err}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let Ok(text) = std::str::from_utf8(&buf[..len]) else {
                continue;
            };
            let Some(found) = parse_announcement(text) else {
                continue;
            };
            if found.motd.contains(MOTD_TAG.trim()) {
                continue; // our own announcement
            }
            // Only announcements sent from this same PC: otherwise a neighbor on the network could
            // slip in the port of any local service (for example, 445), and the host would open it to guests.
            if !is_this_pc(from.ip()) {
                debug!("объявление от {from} не с этого ПК — пропущено");
                continue;
            }
            if retry_after
                .get(&found.port)
                .is_some_and(|at| tokio::time::Instant::now() < *at)
            {
                continue;
            }
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, found.port));
            match status::query(addr, status::DEFAULT_TIMEOUT).await {
                Ok(status) => {
                    return LocalWorld {
                        announcement: found,
                        status,
                    };
                }
                Err(err) => {
                    // The first time a warning, repeated failures only go to the verbose log.
                    if retry_after.contains_key(&found.port) {
                        debug!(
                            "порт {} снова не ответил как Minecraft: {err:#}",
                            found.port
                        );
                    } else {
                        warn!(
                            "порт {} из LAN-объявления пока пропущен: {err:#}",
                            found.port
                        );
                    }
                    retry_after.insert(found.port, tokio::time::Instant::now() + RETRY_AFTER);
                }
            }
        }
    };
    Ok(tokio::time::timeout(wait, search).await.ok())
}

/// Whether the address belongs to this computer. A socket can only be bound to its own address,
/// so a trial bind (without sending data) answers the question reliably.
fn is_this_pc(ip: IpAddr) -> bool {
    ip.is_loopback() || std::net::UdpSocket::bind(SocketAddr::new(ip, 0)).is_ok()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse() {
        let text = format_announcement("Мой мир", 51234);
        assert_eq!(text, "[MOTD]Мой мир [Phase][/MOTD][AD]51234[/AD]");
        let parsed = parse_announcement(&text).unwrap();
        assert_eq!(parsed.port, 51234);
        assert_eq!(parsed.motd, "Мой мир [Phase]");
    }

    #[test]
    fn hostile_motd_cannot_break_format() {
        let text = format_announcement("x[/MOTD][AD]1[/AD]\n", 40000);
        let parsed = parse_announcement(&text).unwrap();
        assert_eq!(parsed.port, 40000);
        let name = parsed.motd.strip_suffix(MOTD_TAG).unwrap();
        assert_eq!(name, "x(/MOTD)(AD)1(/AD)");
    }

    #[test]
    fn parses_vanilla_announcement() {
        let found = parse_announcement("[MOTD]Steve - New World[/MOTD][AD]61234[/AD]").unwrap();
        assert_eq!(found.motd, "Steve - New World");
        assert_eq!(found.port, 61234);
        assert!(parse_announcement("[MOTD]no port[/MOTD]").is_none());
        assert!(parse_announcement("garbage").is_none());
    }

    #[test]
    fn foreign_addresses_are_not_this_pc() {
        assert!(is_this_pc(IpAddr::from(Ipv4Addr::LOCALHOST)));
        // An address from the documentation range (RFC 5737) definitely does not belong to this PC.
        assert!(!is_this_pc(IpAddr::from(Ipv4Addr::new(203, 0, 113, 7))));
    }

    #[test]
    fn mode_from_str() {
        assert_eq!("UNICAST".parse::<AnnounceMode>(), Ok(AnnounceMode::Unicast));
        assert_eq!("both".parse::<AnnounceMode>(), Ok(AnnounceMode::Both));
        assert!("broadcast".parse::<AnnounceMode>().is_err());
    }

    #[tokio::test]
    async fn announces_only_while_online() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = AnnounceTarget {
            unicast: receiver.local_addr().unwrap(),
            ..AnnounceTarget::default()
        };
        let (online_tx, online_rx) = watch::channel(false);
        let task = tokio::spawn(run_announcer(
            AnnounceMode::Unicast,
            target,
            Announcement {
                motd: "Тест".into(),
                port: 55555,
            },
            online_rx,
        ));

        let mut buf = [0u8; 256];
        let silent =
            tokio::time::timeout(Duration::from_millis(400), receiver.recv_from(&mut buf)).await;
        assert!(
            silent.is_err(),
            "there must be no announcements without a connection to the host"
        );

        online_tx.send(true).unwrap();
        let (len, from) =
            tokio::time::timeout(Duration::from_secs(3), receiver.recv_from(&mut buf))
                .await
                .expect("no announcement arrived")
                .unwrap();
        assert!(from.ip().is_loopback(), "the source must be 127.0.0.1");
        let found = parse_announcement(std::str::from_utf8(&buf[..len]).unwrap()).unwrap();
        assert_eq!(found.port, 55555);

        drop(online_tx);
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("announcements must stop after the guest finishes")
            .unwrap()
            .unwrap();
    }
}
