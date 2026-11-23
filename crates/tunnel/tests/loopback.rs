//! End-to-end tests on one computer: host and guest talk over 127.0.0.1
//! (`LoopbackOnly` mode), without relay and internet.

use std::{sync::Arc, time::Duration};

use phase_tunnel::{
    EndpointAddr, EndpointId, SecretKey,
    access::{AccessHook, AccessList, Approver},
    echo,
    endpoint::{self, EndpointOptions, NetworkMode},
    guest::{self, GuestConfig, NotAllowed},
    host::{self, HostConfig},
    metrics,
    voice::{self, VoicePorts},
    wire::RejectReason,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const STATUS: Duration = Duration::from_secs(60);

/// A simple TCP echo server instead of Minecraft. Returns its port.
async fn spawn_fake_game() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = sock.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    port
}

/// Starts a host. Returns its address for the guest.
async fn spawn_host(allowed: Vec<EndpointId>, game_port: Option<u16>) -> EndpointAddr {
    spawn_host_with_approver(allowed, game_port, None).await
}

async fn spawn_host_with_approver(
    allowed: Vec<EndpointId>,
    game_port: Option<u16>,
    approver: Option<Arc<dyn Approver>>,
) -> EndpointAddr {
    spawn_host_full(allowed, game_port, Vec::new(), approver).await
}

async fn spawn_host_full(
    allowed: Vec<EndpointId>,
    game_port: Option<u16>,
    udp_ports: Vec<u16>,
    approver: Option<Arc<dyn Approver>>,
) -> EndpointAddr {
    start_host(
        SecretKey::generate(),
        allowed,
        game_port,
        udp_ports,
        approver,
    )
    .await
    .1
}

/// Starts a host with the given key. Returns its endpoint (to "turn off" the host)
/// and the address for the guest.
async fn start_host(
    key: SecretKey,
    allowed: Vec<EndpointId>,
    game_port: Option<u16>,
    udp_ports: Vec<u16>,
    approver: Option<Arc<dyn Approver>>,
) -> (iroh::Endpoint, EndpointAddr) {
    let access = AccessList::in_memory(allowed);
    let ep = endpoint::bind(EndpointOptions {
        secret_key: key,
        accept_incoming: true,
        network: NetworkMode::LoopbackOnly,

        relay: Default::default(),
        access: Some(AccessHook::new(access.clone(), approver)),
    })
    .await
    .unwrap();
    let addr = endpoint::shareable_addr(&ep, NetworkMode::LoopbackOnly, Duration::ZERO).await;
    let cfg = HostConfig {
        game_port: game_port.into(),
        motd: "Тестовый мир".into(),
        nickname: None,
        udp_ports,
        game_version: Some("1.20.1".into()),
        check_voice_owner: true,
        status_every: STATUS,
        mods: Default::default(),
        room: Default::default(),
    };
    tokio::spawn(host::serve(
        ep.clone(),
        access,
        cfg,
        metrics::Telemetry::new(),
    ));
    (ep, addr)
}

fn guest_config(host: EndpointAddr, voice: VoicePorts) -> GuestConfig {
    GuestConfig {
        host,
        nickname: None,
        entitlement: None,
        local_port: None,
        voice,
        status_every: STATUS,
    }
}

/// A UDP echo server instead of a voice mod. Returns its port and a counter of received packets.
async fn spawn_fake_voice_server() -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let received = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = received.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let Ok((len, from)) = socket.recv_from(&mut buf).await else {
                continue;
            };
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = socket.send_to(&buf[..len], from).await;
        }
    });
    (port, received)
}

async fn guest_endpoint(key: SecretKey) -> iroh::Endpoint {
    endpoint::bind(EndpointOptions {
        secret_key: key,
        accept_incoming: false,
        network: NetworkMode::LoopbackOnly,

        relay: Default::default(),
        access: None,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn game_traffic_goes_through_tunnel() {
    let game_port = spawn_fake_game().await;
    let guest_key = SecretKey::generate();
    let host_addr = spawn_host(vec![guest_key.public()], Some(game_port)).await;

    let ep = guest_endpoint(guest_key).await;
    let handle = guest::start(
        ep,
        guest_config(host_addr, VoicePorts::Off),
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();
    assert_eq!(handle.host_info.motd, "Тестовый мир");
    assert_eq!(handle.host_info.game_version.as_deref(), Some("1.20.1"));
    assert!(
        handle.local_addr.ip().is_loopback(),
        "the guest listens on 127.0.0.1 only"
    );
    // The guest does not take the host's port, it takes any free one.
    assert_ne!(handle.local_addr.port(), game_port);

    // Two "players" at once: each connection is a separate stream.
    let mut tasks = Vec::new();
    for n in 0..2 {
        let addr = handle.local_addr;
        tasks.push(tokio::spawn(async move {
            let mut sock = TcpStream::connect(addr).await.unwrap();
            let payload: Vec<u8> = (0..200_000u32).map(|i| (i * 7 + n) as u8).collect();
            sock.write_all(&payload).await.unwrap();
            sock.shutdown().await.unwrap(); // a close on our side must reach the "game"
            let mut back = Vec::new();
            tokio::time::timeout(Duration::from_secs(10), sock.read_to_end(&mut back))
                .await
                .expect("no answer arrived")
                .unwrap();
            assert_eq!(back, payload);
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
}

#[tokio::test]
async fn unknown_device_is_rejected() {
    let game_port = spawn_fake_game().await;
    // Someone else is on the allow list, not our guest.
    let host_addr = spawn_host(vec![SecretKey::generate().public()], Some(game_port)).await;

    let ep = guest_endpoint(SecretKey::generate()).await;
    let result = guest::start(
        ep,
        guest_config(host_addr, VoicePorts::Off),
        metrics::Telemetry::new(),
    )
    .await;
    let err = result.err().expect("an unknown device must not connect");
    let reason = err.downcast_ref::<NotAllowed>().map(|e| e.reason);
    assert_eq!(
        reason,
        Some(Some(RejectReason::NotAllowed)),
        "expected a host rejection, got: {err:#}"
    );
}

/// A "host" that thinks about the question for the given time and says "no".
#[derive(Debug)]
struct SlowNo(Duration);

impl Approver for SlowNo {
    fn approve(
        &self,
        _id: EndpointId,
        _nickname: Option<String>,
    ) -> n0_future::boxed::BoxFuture<bool> {
        let delay = self.0;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            false
        })
    }
}

/// The host thought and said "no": the guest learns about the rejection right after the answer instead of hanging
/// in "connecting" until its timeout.
#[tokio::test]
async fn guest_learns_about_denial_right_after_host_answers() {
    for think in [Duration::from_millis(300), Duration::from_secs(4)] {
        let host_addr = spawn_host_with_approver(vec![], None, Some(Arc::new(SlowNo(think)))).await;
        let ep = guest_endpoint(SecretKey::generate()).await;
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            think + Duration::from_secs(5),
            guest::connect_once(&ep, &host_addr, None),
        )
        .await;
        let Ok(result) = result else {
            panic!(
                "the host said 'no' after {think:?}, but the guest did not learn it within 5 s of the answer"
            );
        };
        let Err(err) = result else {
            panic!("the host said 'no', but the guest connected");
        };
        let reason = err.downcast_ref::<NotAllowed>().map(|e| e.reason);
        assert_eq!(
            reason,
            Some(Some(RejectReason::Denied)),
            "pause {think:?}: {err:#}"
        );
        assert!(
            started.elapsed() >= think,
            "the rejection is not earlier than the host's answer"
        );
        // After the rejection the guest closes its endpoint: this must not hang either.
        tokio::time::timeout(Duration::from_secs(5), ep.close())
            .await
            .unwrap_or_else(|_| {
                panic!("pause {think:?}: closing the guest endpoint after the rejection hung")
            });
    }
}

/// A "host" that says "yes", but thinks for a second and a half.
#[derive(Debug)]
struct SlowYes;

impl Approver for SlowYes {
    fn approve(
        &self,
        _id: EndpointId,
        _nickname: Option<String>,
    ) -> n0_future::boxed::BoxFuture<bool> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            true
        })
    }
}

#[tokio::test]
async fn only_one_new_device_waits_for_approval() {
    let host_addr = spawn_host_with_approver(vec![], None, Some(Arc::new(SlowYes))).await;

    // Device A waits for the host's decision.
    let first = tokio::spawn({
        let host = host_addr.clone();
        async move {
            let ep = guest_endpoint(SecretKey::generate()).await;
            guest::connect_once(&ep, &host, None).await.map(|_| ())
        }
    });
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Device B meanwhile gets a rejection right away instead of queuing.
    let ep = guest_endpoint(SecretKey::generate()).await;
    let started = std::time::Instant::now();
    let Err(err) = guest::connect_once(&ep, &host_addr, None).await else {
        panic!("the second new device should be rejected");
    };
    let reason = err.downcast_ref::<NotAllowed>().map(|e| e.reason);
    assert_eq!(reason, Some(Some(RejectReason::Busy)), "{err:#}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the rejection should be immediate"
    );

    first
        .await
        .unwrap()
        .expect("the host approved the first device");
}

/// A "host" that never answers; counts how many times it was asked.
#[derive(Debug, Clone, Default)]
struct NeverAnswers {
    asked: Arc<std::sync::atomic::AtomicUsize>,
}

impl Approver for NeverAnswers {
    fn approve(
        &self,
        _id: EndpointId,
        _nickname: Option<String>,
    ) -> n0_future::boxed::BoxFuture<bool> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}

#[tokio::test]
async fn question_is_cancelled_when_device_leaves() {
    let approver = NeverAnswers::default();
    let host_addr = spawn_host_with_approver(vec![], None, Some(Arc::new(approver.clone()))).await;

    // Device A waits for an answer, but leaves (closes the program) without waiting.
    let ep_a = guest_endpoint(SecretKey::generate()).await;
    let attempt = tokio::time::timeout(
        Duration::from_millis(500),
        guest::connect_once(&ep_a, &host_addr, None),
    )
    .await;
    assert!(attempt.is_err(), "the host does not answer, A should wait");
    ep_a.close().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Device B does not get "busy": the question about A is canceled, and now B is asked about.
    let ep_b = guest_endpoint(SecretKey::generate()).await;
    let attempt = tokio::time::timeout(
        Duration::from_millis(800),
        guest::connect_once(&ep_b, &host_addr, None),
    )
    .await;
    assert!(
        attempt.is_err(),
        "B should wait for an answer, not get a 'busy' rejection"
    );
    assert_eq!(approver.asked.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn echo_measures_round_trip() {
    let guest_key = SecretKey::generate();
    // A host without a game port: only diagnostics work.
    let host_addr = spawn_host(vec![guest_key.public()], None).await;

    let ep = guest_endpoint(guest_key).await;
    let (conn, info) = guest::connect_once(&ep, &host_addr, None).await.unwrap();
    assert_eq!(info.game_port, None);
    let report = echo::run(&conn, 20, Duration::from_millis(5))
        .await
        .unwrap();
    assert_eq!(report.received, 20);
    assert_eq!(report.unanswered, 0);
    assert!(report.error.is_none() && !report.timed_out, "{report:?}");
    assert!(
        report.p50_ms < 100.0,
        "the ping on 127.0.0.1 should be small: {report:?}"
    );
}

#[tokio::test]
async fn guest_without_game_port_gets_clear_error() {
    let guest_key = SecretKey::generate();
    let host_addr = spawn_host(vec![guest_key.public()], None).await;
    let ep = guest_endpoint(guest_key).await;
    let err = guest::start(
        ep,
        guest_config(host_addr, VoicePorts::Off),
        metrics::Telemetry::new(),
    )
    .await
    .err()
    .expect("without a game port the guest must not start");
    assert!(format!("{err:#}").contains("--port"));
}

/// Sends a UDP packet to the guest's voice port and waits for the "voice server" reply.
async fn voice_roundtrip(client: &tokio::net::UdpSocket, to: std::net::SocketAddr, payload: &[u8]) {
    client.send_to(payload, to).await.unwrap();
    let mut buf = vec![0u8; 16 * 1024];
    let (len, from) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
        .await
        .expect("the voice server reply did not arrive")
        .unwrap();
    assert_eq!(
        from, to,
        "the reply should come from the address we sent to"
    );
    assert_eq!(&buf[..len], payload);
}

#[tokio::test]
async fn voice_udp_goes_through_tunnel() {
    let game_port = spawn_fake_game().await;
    let (voice_port, _) = spawn_fake_voice_server().await;
    let guest_key = SecretKey::generate();
    let host_addr = spawn_host_full(
        vec![guest_key.public()],
        Some(game_port),
        vec![voice_port],
        None,
    )
    .await;

    // Host and guest on one PC: the host's voice port is taken by the "server", so
    // the guest opens another local port (0 means any free one).
    let ep = guest_endpoint(guest_key).await;
    let handle = guest::start(
        ep,
        guest_config(host_addr, VoicePorts::Map(vec![(voice_port, 0)])),
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();
    assert_eq!(handle.host_info.udp_ports, vec![voice_port]);
    assert_eq!(handle.voice_bound.len(), 1, "{:?}", handle.voice_skipped);
    let local = handle.voice_bound[0].local;
    assert!(local.ip().is_loopback());

    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // A normal voice packet.
    voice_roundtrip(&client, local, b"hello voice").await;
    // A big packet: it does not fit in one datagram and is split into parts.
    let big: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    voice_roundtrip(&client, local, &big).await;

    let stats = handle.voice_stats().snapshot();
    assert!(
        stats.fragmented >= 1,
        "the big packet should have been split: {stats:?}"
    );
    assert_eq!(stats.received, 2, "{stats:?}");
}

#[tokio::test]
async fn host_ignores_voice_for_ports_it_did_not_announce() {
    let (announced, announced_hits) = spawn_fake_voice_server().await;
    let (secret, secret_hits) = spawn_fake_voice_server().await;
    // The "game" and voice servers are in one process (the test process), like a mod in the game.
    let game_port = spawn_fake_game().await;
    let guest_key = SecretKey::generate();
    let host_addr = spawn_host_full(
        vec![guest_key.public()],
        Some(game_port),
        vec![announced],
        None,
    )
    .await;

    let ep = guest_endpoint(guest_key).await;
    let (conn, info) = guest::connect_once(&ep, &host_addr, None).await.unwrap();
    assert_eq!(info.udp_ports, vec![announced]);

    // The guest tries to send a packet to a port the host did not announce.
    conn.send_datagram(voice::encode_datagram(secret, 40000, b"sneaky"))
        .unwrap();
    // And for comparison, to an announced one.
    conn.send_datagram(voice::encode_datagram(announced, 40000, b"ok"))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    use std::sync::atomic::Ordering::SeqCst;
    assert_eq!(
        announced_hits.load(SeqCst),
        1,
        "the announced port should get the packet"
    );
    assert_eq!(
        secret_hits.load(SeqCst),
        0,
        "the unannounced port should get nothing"
    );
}

/// An announced voice port is held by a program that does not listen on the world port (here:
/// a host without a game port, and the "voice server" is some program). The host does not forward
/// guest packets to it: so a number freed after a world restart and taken by another
/// program does not become a loophole.
/// Windows only: the port table cannot be read on other OSes.
#[cfg(windows)]
#[tokio::test]
async fn host_does_not_forward_to_a_port_held_by_a_non_game_program() {
    let (port, hits) = spawn_fake_voice_server().await;
    let guest_key = SecretKey::generate();
    let host_addr = spawn_host_full(vec![guest_key.public()], None, vec![port], None).await;

    let ep = guest_endpoint(guest_key).await;
    let (conn, info) = guest::connect_once(&ep, &host_addr, None).await.unwrap();
    assert_eq!(info.udp_ports, vec![port]);
    for _ in 0..3 {
        conn.send_datagram(voice::encode_datagram(port, 40000, b"hello"))
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "another program must not get the guest's packets"
    );
}
