//! Voice mod UDP (Simple Voice Chat, Plasmo Voice) over QUIC datagrams.
//!
//! Both mods send voice to the same IP as the game (`127.0.0.1` on the guest), to the port
//! the server reports (the voice server port on the host's PC). So:
//! - the guest opens the same UDP ports on `127.0.0.1` as the host's voice ports;
//! - every UDP packet from the game goes to the host as a QUIC datagram with a "port + flow" header
//!   (the flow is the client's source port on the guest);
//! - the host creates **its own** UDP socket for each flow (Simple Voice Chat tells
//!   players apart by sender address) and forwards packets only to the ports it
//!   announced itself: `127.0.0.1:<port>`. The host never takes the voice server port itself
//!   (Simple Voice Chat stops its server if its port is taken).
//!
//! Datagrams are unreliable: a lost voice packet is not resent. For voice
//! this is right: late sound is useless. Packets larger than a datagram are split into
//! parts and reassembled; incomplete packets are dropped.
//!
//! Datagram format (8-byte header + data):
//! `[1: type = 1][2: port][2: flow][2: packet number][1: part (high 4 bits) / total parts (low 4)]`

use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::{BufMut, Bytes, BytesMut};
use iroh::endpoint::Connection;
use serde::Serialize;
use tokio::{net::UdpSocket, sync::watch, task::JoinHandle};
use tracing::{debug, info, warn};

use crate::{host::GameTarget, sockets};

const KIND_UDP: u8 = 1;
const HEADER_LEN: usize = 8;
const MAX_FRAGMENTS: usize = 8;
/// The largest UDP packet we forward. Voice mods send 100–1500 bytes.
pub const MAX_UDP_PAYLOAD: usize = 8 * 1024;
/// How long to wait for missing parts of a packet.
const REASSEMBLY_TIMEOUT: Duration = Duration::from_millis(200);
/// How many partially assembled packets to keep at once.
const MAX_PARTIAL: usize = 64;
/// A flow without packets for longer than this is closed (mods send packets about once a second).
pub const FLOW_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// How many UDP flows one guest can open on the host.
pub const MAX_FLOWS_PER_GUEST: usize = 32;
/// How many clients (source ports) the guest remembers per voice port.
const MAX_CLIENTS: usize = 256;
/// The guest does not open voice ports below this number (system services).
pub const MIN_VOICE_PORT: u16 = 1024;

/// Voice packet counters.
#[derive(Debug, Default)]
pub struct VoiceStats {
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub dropped: AtomicU64,
    pub fragmented: AtomicU64,
}

/// Counter snapshot for the report.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct VoiceCounts {
    pub sent: u64,
    pub received: u64,
    pub dropped: u64,
    pub fragmented: u64,
}

impl VoiceCounts {
    /// Voice was used at all (there were packets).
    pub fn is_used(&self) -> bool {
        self.sent + self.received + self.dropped > 0
    }
}

impl VoiceStats {
    pub fn snapshot(&self) -> VoiceCounts {
        VoiceCounts {
            sent: self.sent.load(Ordering::Relaxed),
            received: self.received.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            fragmented: self.fragmented.load(Ordering::Relaxed),
        }
    }

    fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    port: u16,
    flow: u16,
    msg_id: u16,
    index: u8,
    count: u8,
}

impl Header {
    fn encode(self, payload: &[u8]) -> Bytes {
        let mut buf = BytesMut::with_capacity(HEADER_LEN + payload.len());
        buf.put_u8(KIND_UDP);
        buf.put_u16(self.port);
        buf.put_u16(self.flow);
        buf.put_u16(self.msg_id);
        buf.put_u8((self.index << 4) | (self.count & 0x0f));
        buf.put_slice(payload);
        buf.freeze()
    }

    fn decode(datagram: &Bytes) -> Option<(Self, Bytes)> {
        if datagram.len() < HEADER_LEN || datagram[0] != KIND_UDP {
            return None;
        }
        let u16_at = |i: usize| u16::from_be_bytes([datagram[i], datagram[i + 1]]);
        let frag = datagram[7];
        let header = Self {
            port: u16_at(1),
            flow: u16_at(3),
            msg_id: u16_at(5),
            index: frag >> 4,
            count: frag & 0x0f,
        };
        let valid = header.count >= 1
            && usize::from(header.count) <= MAX_FRAGMENTS
            && header.index < header.count;
        valid.then(|| (header, datagram.slice(HEADER_LEN..)))
    }
}

/// A datagram with one whole UDP packet (for tests and diagnostics).
pub fn encode_datagram(port: u16, flow: u16, payload: &[u8]) -> Bytes {
    Header {
        port,
        flow,
        msg_id: 0,
        index: 0,
        count: 1,
    }
    .encode(payload)
}

/// Sends one UDP packet to the other side (in parts if needed).
fn send_packet(
    conn: &Connection,
    port: u16,
    flow: u16,
    msg_id: u16,
    payload: &[u8],
    stats: &VoiceStats,
) {
    let Some(max) = conn.max_datagram_size() else {
        VoiceStats::inc(&stats.dropped); // datagrams are not supported by this connection
        return;
    };
    let room = max.saturating_sub(HEADER_LEN);
    if room == 0 || payload.len() > MAX_UDP_PAYLOAD {
        VoiceStats::inc(&stats.dropped);
        return;
    }
    let count = payload.len().div_ceil(room).max(1);
    if count > MAX_FRAGMENTS {
        VoiceStats::inc(&stats.dropped);
        return;
    }
    if count > 1 {
        VoiceStats::inc(&stats.fragmented);
    }
    let chunks: Vec<&[u8]> = if payload.is_empty() {
        vec![payload]
    } else {
        payload.chunks(room).collect()
    };
    for (index, chunk) in chunks.into_iter().enumerate() {
        let header = Header {
            port,
            flow,
            msg_id,
            index: index as u8,
            count: count as u8,
        };
        // send_datagram drops the oldest datagrams on overflow:
        // for voice this is better than piling up delay.
        if let Err(err) = conn.send_datagram(header.encode(chunk)) {
            debug!("датаграмма не отправлена: {err}");
            VoiceStats::inc(&stats.dropped);
            return;
        }
    }
    VoiceStats::inc(&stats.sent);
}

/// Reassembling packets from parts.
#[derive(Default)]
struct Reassembler {
    partial: HashMap<(u16, u16, u16), Partial>,
}

struct Partial {
    parts: Vec<Option<Bytes>>,
    have: usize,
    started: Instant,
}

impl Reassembler {
    /// Returns a whole packet once all its parts are collected.
    fn push(&mut self, header: Header, data: Bytes) -> Option<Bytes> {
        if header.count == 1 {
            return Some(data);
        }
        let now = Instant::now();
        self.partial
            .retain(|_, p| now.duration_since(p.started) < REASSEMBLY_TIMEOUT);
        let key = (header.port, header.flow, header.msg_id);
        if !self.partial.contains_key(&key) && self.partial.len() >= MAX_PARTIAL {
            return None;
        }
        let partial = self.partial.entry(key).or_insert_with(|| Partial {
            parts: vec![None; usize::from(header.count)],
            have: 0,
            started: now,
        });
        if partial.parts.len() != usize::from(header.count) {
            self.partial.remove(&key); // parts of one packet do not match, drop it
            return None;
        }
        let slot = &mut partial.parts[usize::from(header.index)];
        if slot.is_none() {
            *slot = Some(data);
            partial.have += 1;
        }
        if partial.have < partial.parts.len() {
            return None;
        }
        let parts = self.partial.remove(&key)?.parts;
        let mut whole = BytesMut::new();
        for part in parts {
            whole.extend_from_slice(&part?);
        }
        Some(whole.freeze())
    }
}

// ---------------------------------------------------------------------------------------
// Host
// ---------------------------------------------------------------------------------------

struct Flow {
    socket: Arc<UdpSocket>,
    last_active: Arc<Mutex<Instant>>,
    task: JoinHandle<()>,
}

type Flows = Arc<Mutex<HashMap<(u16, u16), Arc<Flow>>>>;

/// How long to trust the last port owner check.
const GUARD_TTL: Duration = Duration::from_secs(2);

/// Whether forwarding is allowed to a port whose sockets are held by processes `owners`, if the world
/// port is listened on by process `game_listener`.
///
/// Allowed if nobody holds the port (packets simply go nowhere) or it is held
/// **only** by the process listening on the world port: guests talk to it over TCP anyway. If
/// another program is on the port (for example, it took the number after the world restarted), no.
pub fn owners_allowed(owners: &[u32], game_listener: Option<u32>) -> bool {
    owners.is_empty() || game_listener.is_some_and(|game| owners.iter().all(|&o| o == game))
}

/// Guard of the host's voice ports: checks the port owner not once at startup but the whole
/// time it runs. The voice mod port can change (Simple Voice Chat takes a new
/// random port when the world restarts), and another program can take the freed number:
/// guest packets must not reach it.
#[derive(Debug)]
pub struct PortGuard {
    game_port: GameTarget,
    /// `false` means no checks (the host gave the ports by hand).
    enabled: bool,
    checked: Mutex<HashMap<u16, (Instant, bool)>>,
}

impl PortGuard {
    /// `game_port` is the world (TCP) port on this PC, including its changes; empty means forward
    /// only to ports nobody holds (that is, effectively nowhere).
    pub fn new(game_port: GameTarget) -> Self {
        Self {
            game_port,
            enabled: true,
            checked: Mutex::default(),
        }
    }

    /// Without an owner check: the host gave the ports itself (`--udp`), it is their call.
    /// Forwarding is still only to `127.0.0.1` and only to announced ports.
    pub fn trust_all() -> Self {
        Self {
            game_port: GameTarget::default(),
            enabled: false,
            checked: Mutex::default(),
        }
    }

    /// Whether forwarding to `127.0.0.1:<port>` and back is allowed right now.
    pub fn allows(&self, port: u16) -> bool {
        if !self.enabled {
            return true;
        }
        let now = Instant::now();
        let previous = self.checked.lock().expect("guard lock").get(&port).copied();
        if let Some((at, allowed)) = previous
            && now.duration_since(at) < GUARD_TTL
        {
            return allowed;
        }
        let allowed = self.check(port);
        if previous.map(|(_, was)| was) != Some(allowed) && !allowed {
            warn!(
                "голосовой порт {port} сейчас держит не игра — пакеты гостей туда не пересылаются"
            );
        }
        self.checked
            .lock()
            .expect("guard lock")
            .insert(port, (now, allowed));
        allowed
    }

    fn check(&self, port: u16) -> bool {
        // A port table read error: do not forward (silence is safer).
        let Ok(owners) = phase_netstat::udp_port_owners(port) else {
            return false;
        };
        let listener = self
            .game_port
            .get()
            .and_then(|game| phase_netstat::tcp_listener_owner(game).ok().flatten());
        owners_allowed(&owners, listener)
    }
}
/// Host side: receives guest datagrams and forwards them to the voice server on
/// `127.0.0.1:<port>`, but only for ports from `ports` (the host announced them itself) and only
/// while the game holds the port (see [`PortGuard`]). Runs while the connection is open.
pub async fn serve_host(
    conn: Connection,
    ports: Arc<[u16]>,
    guard: Arc<PortGuard>,
    stats: Arc<VoiceStats>,
) {
    let flows: Flows = Arc::default();
    let mut reassembler = Reassembler::default();
    while let Ok(datagram) = conn.read_datagram().await {
        let Some((header, data)) = Header::decode(&datagram) else {
            VoiceStats::inc(&stats.dropped);
            continue;
        };
        if !ports.contains(&header.port) {
            // The guest does not choose where to send: only ports the host announced.
            debug!(
                "датаграмма на необъявленный порт {} — отброшена",
                header.port
            );
            VoiceStats::inc(&stats.dropped);
            continue;
        }
        let Some(packet) = reassembler.push(header, data) else {
            continue;
        };
        VoiceStats::inc(&stats.received);
        if !guard.allows(header.port) {
            VoiceStats::inc(&stats.dropped);
            continue;
        }
        let Some(flow) = host_flow(&conn, &flows, header.port, header.flow, &guard, &stats).await
        else {
            VoiceStats::inc(&stats.dropped);
            continue;
        };
        *flow.last_active.lock().expect("flow lock") = Instant::now();
        // A "port unreachable" error means the voice server is not running yet; that is fine.
        if let Err(err) = flow.socket.send(&packet).await {
            debug!("UDP-пакет в голосовой сервер не отправлен: {err}");
        }
    }
    let remaining: Vec<_> = flows.lock().expect("flows lock").drain().collect();
    for (_, flow) in remaining {
        flow.task.abort();
    }
}

async fn host_flow(
    conn: &Connection,
    flows: &Flows,
    port: u16,
    flow_id: u16,
    guard: &Arc<PortGuard>,
    stats: &Arc<VoiceStats>,
) -> Option<Arc<Flow>> {
    if let Some(flow) = flows.lock().expect("flows lock").get(&(port, flow_id)) {
        return Some(flow.clone());
    }
    if flows.lock().expect("flows lock").len() >= MAX_FLOWS_PER_GUEST {
        warn!("слишком много голосовых потоков от одного гостя — новый поток отклонён");
        return None;
    }
    // A separate socket per flow: the Simple Voice Chat server tells players apart by address.
    let socket = match UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await {
        Ok(socket) => socket,
        Err(err) => {
            warn!("не удалось открыть UDP-сокет для голоса: {err}");
            return None;
        }
    };
    if let Err(err) = socket.connect((Ipv4Addr::LOCALHOST, port)).await {
        warn!("не удалось настроить UDP-сокет для голоса: {err}");
        return None;
    }
    let socket = Arc::new(socket);
    let last_active = Arc::new(Mutex::new(Instant::now()));
    let task = tokio::spawn(host_flow_reader(
        conn.clone(),
        socket.clone(),
        port,
        flow_id,
        last_active.clone(),
        guard.clone(),
        stats.clone(),
        flows.clone(),
    ));
    let flow = Arc::new(Flow {
        socket,
        last_active,
        task,
    });
    flows
        .lock()
        .expect("flows lock")
        .insert((port, flow_id), flow.clone());
    debug!("новый голосовой поток: порт {port}, поток {flow_id}");
    Some(flow)
}

/// Voice server replies → guest.
#[allow(clippy::too_many_arguments)]
async fn host_flow_reader(
    conn: Connection,
    socket: Arc<UdpSocket>,
    port: u16,
    flow_id: u16,
    last_active: Arc<Mutex<Instant>>,
    guard: Arc<PortGuard>,
    stats: Arc<VoiceStats>,
    flows: Flows,
) {
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD + 1];
    let mut msg_id = 0u16;
    loop {
        match tokio::time::timeout(Duration::from_secs(10), socket.recv(&mut buf)).await {
            // A reply from a port that is not held by the game right now is not passed to the guest.
            Ok(Ok(_)) if !guard.allows(port) => VoiceStats::inc(&stats.dropped),
            Ok(Ok(len)) if len <= MAX_UDP_PAYLOAD => {
                *last_active.lock().expect("flow lock") = Instant::now();
                send_packet(&conn, port, flow_id, msg_id, &buf[..len], &stats);
                msg_id = msg_id.wrapping_add(1);
            }
            Ok(Ok(_)) => VoiceStats::inc(&stats.dropped),
            // On Windows "port unreachable" arrives as an error on the next read. That is fine.
            Ok(Err(err)) => debug!("ошибка чтения голосового UDP: {err}"),
            Err(_) => {} // silence: below we check whether it is time to close the flow
        }
        let idle = last_active.lock().expect("flow lock").elapsed();
        if idle > FLOW_IDLE_TIMEOUT || conn.close_reason().is_some() {
            break;
        }
    }
    flows.lock().expect("flows lock").remove(&(port, flow_id));
    debug!("голосовой поток закрыт: порт {port}, поток {flow_id}");
}

// ---------------------------------------------------------------------------------------
// Guest
// ---------------------------------------------------------------------------------------

/// Which UDP ports to open on the guest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum VoicePorts {
    /// The same numbers the host announced (normal mode).
    #[default]
    SameAsHost,
    /// Do not forward voice.
    Off,
    /// Custom numbers: (host port, local port; 0 means any free one). For tests.
    Map(Vec<(u16, u16)>),
}

/// A voice port opened on the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundVoicePort {
    pub host_port: u16,
    pub local: SocketAddr,
}

/// The guest's voice part. Runs while this object is alive.
pub struct GuestVoice {
    pub bound: Vec<BoundVoicePort>,
    /// Ports that could not be opened, and why.
    pub skipped: Vec<(u16, String)>,
    pub stats: Arc<VoiceStats>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for GuestVoice {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// A voice mod client on the guest: where to reply, through which socket and when it was last active.
struct Client {
    addr: SocketAddr,
    socket: Arc<UdpSocket>,
    last_seen: Instant,
}

type Clients = Arc<Mutex<HashMap<(u16, u16), Client>>>;

/// Remembers a client. If the list is full, first drops long-silent ones, then
/// the oldest: a new client (for example, after rejoining the world) always gets
/// into the list, otherwise replies to it would be lost.
fn remember_client(clients: &Clients, key: (u16, u16), addr: SocketAddr, socket: &Arc<UdpSocket>) {
    let mut map = clients.lock().expect("clients lock");
    let now = Instant::now();
    if !map.contains_key(&key) && map.len() >= MAX_CLIENTS {
        map.retain(|_, c| now.duration_since(c.last_seen) < FLOW_IDLE_TIMEOUT);
        if map.len() >= MAX_CLIENTS
            && let Some(oldest) = map.iter().min_by_key(|(_, c)| c.last_seen).map(|(k, _)| *k)
        {
            map.remove(&oldest);
        }
    }
    map.insert(
        key,
        Client {
            addr,
            socket: socket.clone(),
            last_seen: now,
        },
    );
}

/// Ports the guest never opens: 4445 is the Minecraft LAN announcement port
/// (taking it would break the guest's own "LAN" list).
const RESERVED_PORTS: [u16; 1] = [4445];

/// Opens voice ports on the guest (only `127.0.0.1` and `[::1]`) and forwards packets
/// over the current connection to the host.
pub fn start_guest(
    host_ports: &[u16],
    setting: &VoicePorts,
    conn_rx: watch::Receiver<Option<Connection>>,
) -> GuestVoice {
    let stats = Arc::new(VoiceStats::default());
    let clients: Clients = Arc::default();
    let mut voice = GuestVoice {
        bound: Vec::new(),
        skipped: Vec::new(),
        stats: stats.clone(),
        tasks: Vec::new(),
    };
    let pairs: Vec<(u16, u16)> = match setting {
        VoicePorts::Off => return voice,
        VoicePorts::SameAsHost => host_ports.iter().map(|&p| (p, p)).collect(),
        VoicePorts::Map(map) => map
            .iter()
            .copied()
            .filter(|(host, _)| host_ports.contains(host))
            .collect(),
    };

    for (host_port, local_port) in pairs {
        if host_port < MIN_VOICE_PORT {
            voice.skipped.push((
                host_port,
                phase_lang::tr!(
                    "номер меньше 1024 — такие порты не открываем",
                    "the number is below 1024: such ports are not opened"
                )
                .into(),
            ));
            continue;
        }
        if RESERVED_PORTS.contains(&host_port) || RESERVED_PORTS.contains(&local_port) {
            voice.skipped.push((
                host_port,
                phase_lang::tr!(
                    "это служебный порт Minecraft (LAN-объявления) — не открываем",
                    "this is Minecraft's service port (LAN announcements): not opened"
                )
                .into(),
            ));
            continue;
        }
        // The host chose the port number, not us: check that no program on this PC
        // holds it, otherwise we would intercept its traffic.
        if local_port != 0 {
            match sockets::udp_port_in_use(local_port) {
                Ok(None) => {}
                Ok(Some(pid)) => {
                    voice.skipped.push((
                        host_port,
                        phase_lang::tr!(
                            format!("порт уже занят другой программой (номер процесса {pid})"),
                            format!(
                                "the port is already used by another program (process ID {pid})"
                            )
                        ),
                    ));
                    continue;
                }
                Err(err) => {
                    voice.skipped.push((
                        host_port,
                        phase_lang::tr!(
                            format!("не удалось проверить порт: {err}"),
                            format!("could not check the port: {err}")
                        ),
                    ));
                    continue;
                }
            }
        }
        let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, local_port));
        let socket = match sockets::bind_udp_exclusive(v4) {
            Ok(socket) => Arc::new(socket),
            Err(err) => {
                voice.skipped.push((
                    host_port,
                    phase_lang::tr!(
                        format!("порт занят другой программой ({err})"),
                        format!("the port is used by another program ({err})")
                    ),
                ));
                continue;
            }
        };
        let local = socket.local_addr().unwrap_or(v4);
        voice.bound.push(BoundVoicePort { host_port, local });
        let mut sockets = vec![socket];
        // Some mods may reach localhost over IPv6, so also open [::1] if possible.
        if let Ok(v6) =
            sockets::bind_udp_exclusive(SocketAddr::from((Ipv6Addr::LOCALHOST, local.port())))
        {
            sockets.push(Arc::new(v6));
        }
        for socket in sockets {
            voice.tasks.push(tokio::spawn(guest_listener(
                socket,
                host_port,
                conn_rx.clone(),
                clients.clone(),
                stats.clone(),
            )));
        }
    }
    if !voice.bound.is_empty() {
        voice
            .tasks
            .push(tokio::spawn(guest_reader_manager(conn_rx, clients, stats)));
    }
    voice
}

/// Game (voice mod) packets → host.
async fn guest_listener(
    socket: Arc<UdpSocket>,
    host_port: u16,
    conn_rx: watch::Receiver<Option<Connection>>,
    clients: Clients,
    stats: Arc<VoiceStats>,
) {
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD + 1];
    let mut msg_id = 0u16;
    loop {
        let (len, from) = match socket.recv_from(&mut buf).await {
            Ok(received) => received,
            // On Windows "port unreachable" (the client closed its socket) arrives as an error.
            Err(err) => {
                debug!("ошибка чтения голосового UDP: {err}");
                continue;
            }
        };
        if !from.ip().is_loopback() || len > MAX_UDP_PAYLOAD {
            VoiceStats::inc(&stats.dropped);
            continue;
        }
        let flow = from.port();
        remember_client(&clients, (host_port, flow), from, &socket);
        let conn = conn_rx.borrow().clone();
        match conn {
            Some(conn) => {
                send_packet(&conn, host_port, flow, msg_id, &buf[..len], &stats);
                msg_id = msg_id.wrapping_add(1);
            }
            None => VoiceStats::inc(&stats.dropped), // no connection to the host
        }
    }
}

/// For each new connection to the host, starts receiving its datagrams.
async fn guest_reader_manager(
    mut conn_rx: watch::Receiver<Option<Connection>>,
    clients: Clients,
    stats: Arc<VoiceStats>,
) {
    loop {
        let current = conn_rx.borrow_and_update().clone();
        let reader =
            current.map(|conn| tokio::spawn(guest_reader(conn, clients.clone(), stats.clone())));
        let changed = conn_rx.changed().await;
        if let Some(reader) = reader {
            reader.abort();
        }
        if changed.is_err() {
            return;
        }
    }
}

/// Voice server replies (from the host) → the game on this PC.
async fn guest_reader(conn: Connection, clients: Clients, stats: Arc<VoiceStats>) {
    let mut reassembler = Reassembler::default();
    while let Ok(datagram) = conn.read_datagram().await {
        let Some((header, data)) = Header::decode(&datagram) else {
            VoiceStats::inc(&stats.dropped);
            continue;
        };
        let Some(packet) = reassembler.push(header, data) else {
            continue;
        };
        let target = clients
            .lock()
            .expect("clients lock")
            .get(&(header.port, header.flow))
            .map(|c| (c.addr, c.socket.clone()));
        let Some((addr, socket)) = target else {
            VoiceStats::inc(&stats.dropped);
            continue;
        };
        VoiceStats::inc(&stats.received);
        if let Err(err) = socket.send_to(&packet, addr).await {
            debug!("UDP-пакет игре не отправлен: {err}");
        }
    }
}

/// Logs the voice summary if voice was used at all.
pub fn log_summary(label: &str, stats: &VoiceStats) {
    let c = stats.snapshot();
    if c.is_used() {
        info!(
            "[{label}] голос: отправлено {}, получено {}, отброшено {}, разрезано на части {}",
            c.sent, c.received, c.dropped, c.fragmented
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarding_only_to_free_ports_or_the_games_ports() {
        let game = Some(100);
        assert!(owners_allowed(&[], game), "nobody holds it: allowed");
        assert!(owners_allowed(&[], None));
        assert!(owners_allowed(&[100], game), "held by the game: allowed");
        assert!(
            !owners_allowed(&[200], game),
            "held by another program: not allowed"
        );
        assert!(
            !owners_allowed(&[100, 200], game),
            "the game and another program together: not allowed"
        );
        assert!(!owners_allowed(&[100], None), "game unknown: not allowed");
    }

    #[cfg(windows)]
    #[test]
    fn guard_checks_owner_unless_host_chose_ports_himself() {
        // This process holds the port, and the world port is not set: the "game" is unknown.
        let busy = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = busy.local_addr().unwrap().port();
        assert!(!PortGuard::new(GameTarget::default()).allows(port));
        // The same process listens on the world port, so the voice port is held by the "game".
        let game = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let game_port = game.local_addr().unwrap().port();
        assert!(PortGuard::new(game_port.into()).allows(port));
        // The host chose the --udp ports itself: no check.
        assert!(PortGuard::trust_all().allows(port));
    }
    fn header(index: u8, count: u8) -> Header {
        Header {
            port: 24454,
            flow: 50000,
            msg_id: 7,
            index,
            count,
        }
    }

    #[test]
    fn header_roundtrip_and_validation() {
        let h = header(2, 3);
        let datagram = h.encode(b"abc");
        let (decoded, data) = Header::decode(&datagram).unwrap();
        assert_eq!(decoded, h);
        assert_eq!(&data[..], b"abc");

        // Invalid datagrams are dropped.
        assert!(Header::decode(&Bytes::from_static(b"short")).is_none());
        assert!(
            Header::decode(&header(3, 3).encode(b"x")).is_none(),
            "part number ≥ total"
        );
        assert!(
            Header::decode(&header(0, 0).encode(b"x")).is_none(),
            "0 parts"
        );
        let mut wrong_kind = BytesMut::from(&header(0, 1).encode(b"x")[..]);
        wrong_kind[0] = 99;
        assert!(Header::decode(&wrong_kind.freeze()).is_none());
    }

    #[test]
    fn reassembles_out_of_order() {
        let mut r = Reassembler::default();
        assert!(r.push(header(2, 3), Bytes::from_static(b"C")).is_none());
        assert!(r.push(header(0, 3), Bytes::from_static(b"A")).is_none());
        assert!(
            r.push(header(0, 3), Bytes::from_static(b"A")).is_none(),
            "a duplicate does not break reassembly"
        );
        let whole = r.push(header(1, 3), Bytes::from_static(b"B")).unwrap();
        assert_eq!(&whole[..], b"ABC");
        assert!(r.partial.is_empty());
    }

    #[test]
    fn single_part_passes_through() {
        let mut r = Reassembler::default();
        let whole = r.push(header(0, 1), Bytes::from_static(b"voice")).unwrap();
        assert_eq!(&whole[..], b"voice");
    }

    #[test]
    fn partial_store_is_bounded() {
        let mut r = Reassembler::default();
        for id in 0..(MAX_PARTIAL as u16 + 10) {
            let h = Header {
                msg_id: id,
                ..header(0, 2)
            };
            r.push(h, Bytes::from_static(b"x"));
        }
        assert!(r.partial.len() <= MAX_PARTIAL);
    }

    #[tokio::test]
    async fn guest_never_opens_minecraft_lan_port() {
        let (_tx, rx) = watch::channel(None);
        let voice = start_guest(&[4445], &VoicePorts::SameAsHost, rx);
        assert!(voice.bound.is_empty());
        assert_eq!(voice.skipped.len(), 1);
    }

    #[tokio::test]
    async fn client_list_makes_room_for_new_clients() {
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let clients: Clients = Arc::default();
        let addr = |port: u16| SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        for flow in 0..MAX_CLIENTS as u16 {
            remember_client(&clients, (24454, flow), addr(10_000 + flow), &socket);
        }
        // The list is full, but a new client (rejoined the world with a new port) still gets in.
        remember_client(&clients, (24454, 60_000), addr(60_000), &socket);
        let map = clients.lock().unwrap();
        assert!(map.len() <= MAX_CLIENTS);
        assert!(map.contains_key(&(24454, 60_000)));
        assert!(!map.contains_key(&(24454, 0)), "the oldest was evicted");
    }

    #[tokio::test]
    async fn guest_skips_low_ports() {
        let (_tx, rx) = watch::channel(None);
        let voice = start_guest(&[80], &VoicePorts::SameAsHost, rx);
        assert!(voice.bound.is_empty());
        assert_eq!(voice.skipped.len(), 1);
    }
}
