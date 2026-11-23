//! Start of the game stream: the host shows it to its program (the guest's game nickname), and the bytes
//! go to the game unchanged. Also the styling stream: the guest asks how it looks
//! in the world and toggles the glow on itself. Everything on 127.0.0.1.

mod common;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use phase_tunnel::{
    EndpointAddr, EndpointId, SecretKey,
    access::AccessList,
    endpoint::{self, EndpointOptions, NetworkMode},
    events::{EventSink, TunnelEvent},
    guest::{self, GuestConfig},
    host::{self, GameAdmit, GameSniff, HostConfig, RoomOptions, RoomService},
    metrics::Telemetry,
    voice::VoicePorts,
    wire::{LookRequest, LookState, RoomInfo},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const WAIT: Duration = Duration::from_secs(10);

/// A "host program" with a toy protocol: login is the line `LOGIN <nick>\n`.
#[derive(Debug, Default)]
struct ToyRoom {
    /// Which chunks of the stream start the core showed.
    seen: Mutex<Vec<Vec<u8>>>,
    glow: Mutex<bool>,
}

impl RoomService for ToyRoom {
    fn hello(&self, _peer: &EndpointId, _entitlement: Option<&str>) {}

    fn info(&self, _peer: &EndpointId) -> RoomInfo {
        RoomInfo {
            max_players: 5,
            pro: false,
            look: true,
        }
    }

    fn sniff_game(&self, head: &[u8]) -> GameSniff {
        self.seen.lock().unwrap().push(head.to_vec());
        if !b"LOGIN ".starts_with(&head[..head.len().min(6)]) {
            return GameSniff::Skip;
        }
        match head.iter().position(|b| *b == b'\n') {
            Some(end) if end > 6 => GameSniff::Login {
                name: String::from_utf8_lossy(&head[6..end]).into_owned(),
                uuid: None,
            },
            _ => GameSniff::NeedMore,
        }
    }

    fn admit_game(&self, _peer: &EndpointId, name: &str) -> GameAdmit {
        if name == "Taken" {
            GameAdmit::Refuse {
                reason: "ник занят".into(),
                reply: b"REFUSED Taken\n".to_vec(),
            }
        } else {
            GameAdmit::Allow
        }
    }

    fn look(&self, _peer: &EndpointId, request: LookRequest) -> LookState {
        let mut glow = self.glow.lock().unwrap();
        if let Some(wanted) = request.glow {
            *glow = wanted;
        }
        LookState {
            enabled: true,
            game_name: Some("Steve\u{7}".into()),
            color: Some("ice_sapphire".into()),
            prefix: Some("PRO".into()),
            glow: *glow,
            can_glow: true,
        }
    }
}

struct Host {
    addr: EndpointAddr,
    events: Arc<Mutex<Vec<TunnelEvent>>>,
    room: Arc<ToyRoom>,
}

async fn start_host(game_port: u16, guest: EndpointId) -> Host {
    let events: Arc<Mutex<Vec<TunnelEvent>>> = Arc::default();
    let sink = EventSink::new({
        let events = events.clone();
        move |event| events.lock().unwrap().push(event)
    });
    let access = AccessList::in_memory([guest]);
    let ep = endpoint::bind(EndpointOptions {
        secret_key: SecretKey::generate(),
        accept_incoming: true,
        network: NetworkMode::LoopbackOnly,
        relay: Default::default(),
        access: None,
    })
    .await
    .unwrap();
    let addr = endpoint::shareable_addr(&ep, NetworkMode::LoopbackOnly, Duration::ZERO).await;
    let room = Arc::new(ToyRoom::default());
    let cfg = HostConfig {
        game_port: Some(game_port).into(),
        motd: "Мир".into(),
        nickname: None,
        udp_ports: Vec::new(),
        game_version: None,
        check_voice_owner: true,
        status_every: Duration::from_secs(60),
        mods: Default::default(),
        room: RoomOptions {
            max_players: 5,
            service: Some(room.clone()),
        },
    };
    tokio::spawn(host::serve(ep, access, cfg, Telemetry::with_events(sink)));
    Host { addr, events, room }
}

async fn start_guest(key: SecretKey, host: &Host) -> guest::GuestHandle {
    let ep = endpoint::bind(EndpointOptions {
        secret_key: key,
        accept_incoming: false,
        network: NetworkMode::LoopbackOnly,
        relay: Default::default(),
        access: None,
    })
    .await
    .unwrap();
    let cfg = GuestConfig {
        host: host.addr.clone(),
        nickname: Some("HelloNick".into()),
        entitlement: None,
        local_port: None,
        voice: VoicePorts::Off,
        status_every: Duration::from_secs(60),
    };
    tokio::time::timeout(WAIT, guest::start(ep, cfg, Telemetry::default()))
        .await
        .expect("the guest did not connect in time")
        .unwrap()
}

/// A "game" that, like a Minecraft server, stays silent until the client introduces itself: the echo
/// starts after the first newline or `quiet_until` bytes.
async fn spawn_quiet_game(quiet_until: usize) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut held = Vec::new();
                let mut buf = [0u8; 4096];
                let mut talking = false;
                while let Ok(len) = sock.read(&mut buf).await {
                    if len == 0 {
                        break;
                    }
                    held.extend_from_slice(&buf[..len]);
                    talking |= held.contains(&b'\n') || held.len() >= quiet_until;
                    if talking && sock.write_all(&std::mem::take(&mut held)).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

fn game_names(host: &Host) -> Vec<(String, String)> {
    host.events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            TunnelEvent::PeerGameName { peer, name, .. } => Some((peer.clone(), name.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn game_name_is_read_and_bytes_reach_the_game_unchanged() {
    let game = spawn_quiet_game(usize::MAX).await;
    let key = SecretKey::generate();
    let id = key.public().to_string();
    let host = start_host(game, key.public()).await;
    let guest = start_guest(key, &host).await;

    // The login arrives in two chunks: the core keeps reading until the program learns the nickname.
    let mut sock = TcpStream::connect(guest.local_addr).await.unwrap();
    sock.write_all(b"LOGIN Ste").await.unwrap();
    sock.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    sock.write_all(b"ve_01\nencrypted-from-here").await.unwrap();
    // The game got exactly the bytes the client sent.
    common::read_exactly(&mut sock, b"LOGIN Steve_01\nencrypted-from-here", WAIT).await;
    // After that, the normal pump in both directions.
    common::roundtrip(&mut sock, &[0xAB; 4096], WAIT).await;

    assert_eq!(
        game_names(&host),
        vec![(id.clone(), "Steve_01".to_string())]
    );
    // After the program answers, the core no longer looks into the stream.
    let seen = host.room.seen.lock().unwrap().clone();
    assert!(seen.len() <= 3, "extra parse calls: {}", seen.len());
    assert!(seen.iter().all(|head| head.len() <= host::SNIFF_MAX));

    // Not a game login: bytes go as is, no event.
    let mut other = TcpStream::connect(guest.local_addr).await.unwrap();
    common::roundtrip(&mut other, b"\xFE\x01 not a login\n", WAIT).await;
    common::roundtrip(&mut other, &[7; 2048], WAIT).await;
    assert_eq!(game_names(&host).len(), 1);

    // Every new game connection is read again (the guest rejoined the world).
    let mut again = TcpStream::connect(guest.local_addr).await.unwrap();
    common::roundtrip(&mut again, b"LOGIN Alex\n", WAIT).await;
    assert_eq!(
        game_names(&host),
        vec![
            (id.clone(), "Steve_01".to_string()),
            (id, "Alex".to_string())
        ]
    );
}

#[tokio::test]
async fn unfinished_or_foreign_start_does_not_stall_the_stream() {
    let key = SecretKey::generate();
    // The game is silent for the first 2 KB: the core stops looking after SNIFF_MAX bytes by itself.
    let quiet = spawn_quiet_game(2 * host::SNIFF_MAX).await;
    let host = start_host(quiet, key.public()).await;
    let guest = start_guest(key, &host).await;
    let mut sock = TcpStream::connect(guest.local_addr).await.unwrap();
    let mut data = b"LOGIN ".to_vec();
    data.resize(3 * host::SNIFF_MAX, b'x');
    common::roundtrip(&mut sock, &data, WAIT).await;
    assert!(game_names(&host).is_empty());

    // The "game" spoke first without waiting for the client (echo): this is not Minecraft, reading
    // the stream start stops, data flows both ways.
    let key = SecretKey::generate();
    let echo = common::spawn_fake_game().await;
    let host = start_host(echo, key.public()).await;
    let guest = start_guest(key, &host).await;
    let mut sock = TcpStream::connect(guest.local_addr).await.unwrap();
    common::roundtrip(&mut sock, b"LOGIN St", WAIT).await;
    common::roundtrip(&mut sock, b"eve\n", WAIT).await;
    assert!(game_names(&host).is_empty());
}

/// An echo "game" that counts how many bytes reached it.
async fn spawn_counting_game() -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let got = Arc::new(AtomicUsize::new(0));
    let counter = got.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let counter = counter.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(len) = sock.read(&mut buf).await {
                    if len == 0 {
                        break;
                    }
                    counter.fetch_add(len, Ordering::SeqCst);
                    if sock.write_all(&buf[..len]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    (port, got)
}

/// The host program rejected a nickname: the guest's game gets the program's reply and a close, and
/// not a single byte reaches the host's game. Another nickname from the same device gets through.
#[tokio::test]
async fn refused_name_never_reaches_the_game() {
    use std::sync::atomic::Ordering;
    let (game, got) = spawn_counting_game().await;
    let key = SecretKey::generate();
    let id = key.public().to_string();
    let host = start_host(game, key.public()).await;
    let guest = start_guest(key, &host).await;

    let mut sock = TcpStream::connect(guest.local_addr).await.unwrap();
    sock.write_all(b"LOGIN Taken\nencrypted-from-here")
        .await
        .unwrap();
    common::read_exactly(&mut sock, b"REFUSED Taken\n", WAIT).await;
    let mut rest = Vec::new();
    let closed = tokio::time::timeout(WAIT, sock.read_to_end(&mut rest)).await;
    assert!(
        closed.is_ok(),
        "the guest connection is closed after the rejection"
    );
    assert!(rest.is_empty(), "nothing after the rejection: {rest:?}");
    assert_eq!(
        got.load(Ordering::SeqCst),
        0,
        "the host's game did not see the rejected login"
    );
    let refused: Vec<_> = host
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            TunnelEvent::GameRefused { peer, name, reason } => {
                Some((peer.clone(), name.clone(), reason.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        refused,
        vec![(id.clone(), "Taken".into(), "ник занят".into())]
    );
    assert!(game_names(&host).is_empty());

    // An allowed login: the stream start is held until the decision and goes to the game in full.
    let mut ok = TcpStream::connect(guest.local_addr).await.unwrap();
    common::roundtrip(&mut ok, b"LOGIN Alex\nhello", WAIT).await;
    assert_eq!(game_names(&host), vec![(id, "Alex".to_string())]);
}

#[tokio::test]
async fn guest_toggles_own_glow_over_the_look_stream() {
    let game = common::spawn_fake_game().await;
    let key = SecretKey::generate();
    let host = start_host(game, key.public()).await;
    let guest = start_guest(key, &host).await;
    assert!(guest.host_info.room.is_some_and(|room| room.look));
    let conn = guest.connection().expect("connection to the host");

    let state = guest::look(&conn, LookRequest::default()).await.unwrap();
    assert!(state.enabled && state.can_glow && !state.glow);
    assert_eq!(state.color.as_deref(), Some("ice_sapphire"));
    // Text from the host is stripped of control characters.
    assert_eq!(state.game_name.as_deref(), Some("Steve"));

    let on = guest::look(&conn, LookRequest { glow: Some(true) })
        .await
        .unwrap();
    assert!(on.glow);
    // A question without a command changes nothing.
    let asked = guest::look(&conn, LookRequest::default()).await.unwrap();
    assert!(asked.glow);
    let off = guest::look(&conn, LookRequest { glow: Some(false) })
        .await
        .unwrap();
    assert!(!off.glow);
}
