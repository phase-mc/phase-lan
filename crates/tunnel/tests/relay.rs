//! The tunnel through our own relay (`phase-relay`), started inside the test on
//! 127.0.0.1. Without internet and without n0 relays.

mod common;

use std::{
    net::SocketAddr,
    path::PathBuf,
    time::{Duration, Instant},
};

use common::{Cable, read_exactly, roundtrip, spawn_fake_game, spawn_fake_voice, voice_roundtrip};
use iroh::{TransportAddr, Watcher, endpoint::Connection};
use phase_relay::{
    AccessRules, CertSource, LIST_RELOAD_INTERVAL, RelayOptions, RelayServer, TlsOptions, certs,
};
use phase_tunnel::{
    EndpointAddr, EndpointId, RelayUrl, SecretKey,
    access::{AccessHook, AccessList},
    endpoint::{self, EndpointOptions, NetworkMode, RelaySettings},
    guest::{self, GuestConfig, GuestHandle},
    host::{self, HostConfig},
    metrics,
    voice::VoicePorts,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpStream, UdpSocket},
};

const STATUS: Duration = Duration::from_secs(60);
/// How long to wait for the relay connection.
const ONLINE_WAIT: Duration = Duration::from_secs(10);
const TOKEN: &str = "test-relay-token-0123456789";

fn any_local() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// A non-TLS relay on 127.0.0.1 (like `phase-relay` on a home network).
async fn plain_relay(access: AccessRules) -> RelayServer {
    let mut opts = RelayOptions::plain_http(any_local());
    opts.access = access;
    RelayServer::spawn(opts).await.unwrap()
}

fn own_relay(url: &RelayUrl, token: Option<&str>) -> RelaySettings {
    RelaySettings {
        urls: vec![url.clone()],
        token: token.map(Into::into),
        ..Default::default()
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phase-relay-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn host_config(game_port: u16, udp_ports: Vec<u16>) -> HostConfig {
    HostConfig {
        game_port: Some(game_port).into(),
        motd: "Мир через relay".into(),
        nickname: None,
        udp_ports,
        game_version: None,
        check_voice_owner: true,
        status_every: STATUS,
        mods: Default::default(),
        room: Default::default(),
    }
}

/// A "relay only" host (like `host --relay-only --relay <url>`). Returns the endpoint
/// and the address for the ticket.
async fn start_host(
    key: SecretKey,
    allowed: Vec<EndpointId>,
    game_port: u16,
    udp_ports: Vec<u16>,
    relay: RelaySettings,
) -> (iroh::Endpoint, EndpointAddr) {
    let access = AccessList::in_memory(allowed);
    let ep = endpoint::bind(EndpointOptions {
        secret_key: key,
        accept_incoming: true,
        network: NetworkMode::RelayOnly,
        relay,
        access: Some(AccessHook::new(access.clone(), None)),
    })
    .await
    .unwrap();
    let addr = endpoint::shareable_addr(&ep, NetworkMode::RelayOnly, ONLINE_WAIT).await;
    let cfg = host_config(game_port, udp_ports);
    tokio::spawn(host::serve(
        ep.clone(),
        access,
        cfg,
        metrics::Telemetry::new(),
    ));
    (ep, addr)
}

async fn relay_only_endpoint(key: SecretKey, relay: RelaySettings) -> iroh::Endpoint {
    endpoint::bind(EndpointOptions {
        secret_key: key,
        accept_incoming: false,
        network: NetworkMode::RelayOnly,
        relay,
        access: None,
    })
    .await
    .unwrap()
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

fn selected_path(conn: &Connection) -> Option<TransportAddr> {
    conn.paths()
        .iter()
        .find(|p| p.is_selected())
        .map(|p| p.remote_addr().clone())
}

/// Waits until data goes over the wanted path.
async fn wait_for_path(
    handle: &GuestHandle,
    want: impl Fn(&TransportAddr) -> bool,
    within: Duration,
) -> TransportAddr {
    let deadline = Instant::now() + within;
    loop {
        if let Some(path) = handle.connection().as_ref().and_then(selected_path)
            && want(&path)
        {
            return path;
        }
        assert!(
            Instant::now() < deadline,
            "the wanted path was not selected within {within:?}; now: {:?}",
            handle.connection().as_ref().and_then(selected_path)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Waits until the endpoint has an explanation of why there is no relay connection.
async fn wait_for_relay_problem(ep: &iroh::Endpoint, within: Duration) -> String {
    let deadline = Instant::now() + within;
    loop {
        if let Some(problem) = endpoint::relay_problem(ep) {
            return problem;
        }
        assert!(
            Instant::now() < deadline,
            "no explanation of the missing relay connection appeared within {within:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Game and voice go through our own relay. The guest specifies nothing: it takes the relay from
/// the ticket; n0 relays are used by neither host nor guest.
#[tokio::test]
async fn game_and_voice_go_through_own_relay() {
    let relay = plain_relay(AccessRules::default()).await;
    let url = relay.local_url("");
    let game_port = spawn_fake_game().await;
    let voice_port = spawn_fake_voice().await;
    let (host_key, guest_key) = (SecretKey::generate(), SecretKey::generate());
    let (_host, addr) = start_host(
        host_key,
        vec![guest_key.public()],
        game_port,
        vec![voice_port],
        own_relay(&url, None),
    )
    .await;

    // Ticket: only our relay, no direct addresses ("relay only" mode).
    assert_eq!(
        addr.relay_urls().cloned().collect::<Vec<_>>(),
        vec![url.clone()]
    );
    assert_eq!(addr.ip_addrs().count(), 0);

    let guest_relay = RelaySettings::default().with_relays_of(&addr);
    assert_eq!(guest_relay.peer_urls, vec![url.clone()]);
    let ep = relay_only_endpoint(guest_key, guest_relay).await;
    let handle = guest::start(
        ep.clone(),
        // Host and guest on one PC: the "voice server" port is taken, the guest takes another.
        guest_config(addr, VoicePorts::Map(vec![(voice_port, 0)])),
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();

    let before = relay.relayed_packets();
    let mut game = TcpStream::connect(handle.local_addr).await.unwrap();
    roundtrip(&mut game, b"through-our-relay", Duration::from_secs(5)).await;
    let voice_client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    voice_roundtrip(&voice_client, handle.voice_bound[0].local, b"voice").await;

    assert!(
        relay.relayed_packets() > before,
        "packets should go through our relay"
    );
    let path = selected_path(&handle.connection().unwrap()).unwrap();
    assert_eq!(path, TransportAddr::Relay(url.clone()));
    // The guest picks its home relay in the background (latency probe), so wait for the choice.
    tokio::time::timeout(ONLINE_WAIT, ep.online())
        .await
        .expect("the guest connected to the relay as its home relay");
    let homes: Vec<RelayUrl> = ep
        .home_relay_status()
        .get()
        .iter()
        .map(|s| s.url().clone())
        .collect();
    assert_eq!(homes, vec![url], "the guest's home relay is ours, not n0");
}

/// A relay with HTTPS and a self-signed certificate (a VPS without a domain): whoever trusts
/// the certificate (`--relay-ca`) works through the relay; without the certificate a client does not connect
/// to such a relay: TLS verification is not disabled.
#[tokio::test]
async fn https_relay_with_own_certificate() {
    let dir = temp_dir("https");
    let relay = RelayServer::spawn(RelayOptions {
        http_addr: any_local(),
        tls: Some(TlsOptions {
            https_addr: any_local(),
            cert: CertSource::SelfSigned {
                names: vec!["127.0.0.1".into()],
                dir: dir.clone(),
            },
            quic_addr: Some(any_local()),
        }),
        access: AccessRules::default(),
        client_rate_limit: None,
        metrics_addr: None,
    })
    .await
    .unwrap();
    assert!(relay.https_addr().is_some() && relay.quic_addr().is_some());
    let url = relay.local_url("127.0.0.1");
    assert_eq!(url.scheme(), "https");
    let ca = endpoint::read_ca_file(&dir.join(certs::SELF_SIGNED_CERT_FILE)).unwrap();

    let game_port = spawn_fake_game().await;
    let (host_key, guest_key) = (SecretKey::generate(), SecretKey::generate());
    let mut host_relay = own_relay(&url, None);
    host_relay.extra_roots = ca.clone();
    let (_host, addr) = start_host(
        host_key,
        vec![guest_key.public()],
        game_port,
        Vec::new(),
        host_relay,
    )
    .await;
    assert_eq!(
        addr.relay_urls().cloned().collect::<Vec<_>>(),
        vec![url.clone()]
    );

    // Without the relay certificate the server is not verified: no home relay and no connection
    // to the host through it.
    let stranger_key = SecretKey::generate();
    let stranger = relay_only_endpoint(stranger_key, own_relay(&url, None)).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), stranger.online())
            .await
            .is_err(),
        "a relay with an untrusted certificate must not be used"
    );
    let attempt = tokio::time::timeout(
        Duration::from_secs(10),
        guest::connect_once(&stranger, &addr, None),
    )
    .await;
    assert!(
        !matches!(attempt, Ok(Ok(_))),
        "without the relay certificate there is no connection"
    );
    stranger.close().await;

    // With the certificate: the game goes through the HTTPS relay.
    let guest_relay = RelaySettings {
        extra_roots: ca,
        ..Default::default()
    }
    .with_relays_of(&addr);
    let ep = relay_only_endpoint(guest_key, guest_relay).await;
    let handle = guest::start(
        ep,
        guest_config(addr, VoicePorts::Off),
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();
    let mut game = TcpStream::connect(handle.local_addr).await.unwrap();
    roundtrip(&mut game, b"https-relay", Duration::from_secs(5)).await;
    assert_eq!(
        selected_path(&handle.connection().unwrap()),
        Some(TransportAddr::Relay(url))
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relay with a password: without the password it does not let in and explains it clearly; with the password
/// (host and guest pass `--relay-token`) everything works.
#[tokio::test]
async fn relay_password_is_required() {
    let relay = plain_relay(AccessRules {
        tokens: vec![TOKEN.into()],
        allow_file: None,
    })
    .await;
    let url = relay.local_url("");
    let game_port = spawn_fake_game().await;
    let (host_key, guest_key, nopass_key) = (
        SecretKey::generate(),
        SecretKey::generate(),
        SecretKey::generate(),
    );
    let (_host, addr) = start_host(
        host_key,
        vec![guest_key.public(), nopass_key.public()],
        game_port,
        Vec::new(),
        own_relay(&url, Some(TOKEN)),
    )
    .await;
    assert_eq!(
        addr.relay_urls().count(),
        1,
        "the host with the password connected to the relay"
    );

    // A guest without the password: the relay refuses, the program explains why.
    let nopass =
        relay_only_endpoint(nopass_key, RelaySettings::default().with_relays_of(&addr)).await;
    let problem = wait_for_relay_problem(&nopass, ONLINE_WAIT).await;
    assert!(problem.contains("отказал в доступе"), "{problem}");
    let attempt = tokio::time::timeout(
        Duration::from_secs(10),
        guest::start(
            nopass.clone(),
            guest_config(addr.clone(), VoicePorts::Off),
            metrics::Telemetry::new(),
        ),
    )
    .await;
    assert!(
        !matches!(attempt, Ok(Ok(_))),
        "without the relay password there is no connection"
    );
    nopass.close().await;

    // A guest with the password.
    let ep = relay_only_endpoint(
        guest_key,
        RelaySettings {
            token: Some(TOKEN.into()),
            ..Default::default()
        }
        .with_relays_of(&addr),
    )
    .await;
    let handle = guest::start(
        ep,
        guest_config(addr, VoicePorts::Off),
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();
    let mut game = TcpStream::connect(handle.local_addr).await.unwrap();
    roundtrip(&mut game, b"with-password", Duration::from_secs(5)).await;
}

/// The relay device list is reread on the fly: an added device is let in,
/// a removed one is disconnected and can no longer connect.
#[tokio::test]
async fn relay_allow_list_admits_and_revokes_devices() {
    let file = temp_dir("allow").join("allowed.txt");
    let (host_key, guest_key) = (SecretKey::generate(), SecretKey::generate());
    std::fs::write(&file, format!("{}  # хост\n", host_key.public())).unwrap();
    let relay = plain_relay(AccessRules {
        tokens: Vec::new(),
        allow_file: Some(file.clone()),
    })
    .await;
    let url = relay.local_url("");

    let host = relay_only_endpoint(host_key.clone(), own_relay(&url, None)).await;
    tokio::time::timeout(ONLINE_WAIT, host.online())
        .await
        .expect("the host is on the list, the relay lets it in");

    // The guest is not on the list: rejected.
    let denied = relay_only_endpoint(guest_key.clone(), own_relay(&url, None)).await;
    let problem = wait_for_relay_problem(&denied, ONLINE_WAIT).await;
    assert!(problem.contains("отказал в доступе"), "{problem}");
    denied.close().await;

    // The guest was added (the file changes while the relay runs): let in.
    std::fs::write(
        &file,
        format!("{}\n{}  # гость\n", host_key.public(), guest_key.public()),
    )
    .unwrap();
    tokio::time::sleep(LIST_RELOAD_INTERVAL * 2).await;
    let guest = relay_only_endpoint(guest_key.clone(), own_relay(&url, None)).await;
    tokio::time::timeout(ONLINE_WAIT, guest.online())
        .await
        .expect("the guest was added to the list, the relay lets it in");
    assert!(relay.is_connected(&guest_key.public()));

    // The guest was removed: the relay disconnects it and does not let it back; the host stays.
    std::fs::write(&file, format!("{}\n", host_key.public())).unwrap();
    let deadline = Instant::now() + LIST_RELOAD_INTERVAL * 4;
    while relay.is_connected(&guest_key.public()) {
        assert!(
            Instant::now() < deadline,
            "a device removed from the list should be disconnected"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        !relay.is_connected(&guest_key.public()),
        "a disconnected device cannot connect again"
    );
    assert!(relay.is_connected(&host_key.public()));
}

/// Test T6 for two paths: data goes direct, the direct path disappears, and after
/// a few seconds it goes through our relay, and the game does not disconnect.
///
/// The host listens on `[::1]`, the guest on `127.0.0.1`: the direct path exists only through the "cable"
/// (see `common::Cable`), and the relay is the fallback path.
#[tokio::test]
async fn direct_path_fails_over_to_own_relay() {
    let relay = plain_relay(AccessRules::default()).await;
    let url = relay.local_url("");
    let game_port = spawn_fake_game().await;
    let (host_key, guest_key) = (SecretKey::generate(), SecretKey::generate());

    let access = AccessList::in_memory([guest_key.public()]);
    let host = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(host_key.clone())
        .transport_config(endpoint::transport_config())
        .relay_mode(own_relay(&url, None).mode())
        .clear_ip_transports()
        .bind_addr("[::1]:0")
        .unwrap()
        .alpns(vec![phase_tunnel::wire::ALPN.to_vec()])
        .hooks(AccessHook::new(access.clone(), None))
        .bind()
        .await
        .unwrap();
    tokio::time::timeout(ONLINE_WAIT, host.online())
        .await
        .expect("the host connected to the relay");
    let host_socket = *host
        .bound_sockets()
        .iter()
        .find(|a| a.is_ipv6())
        .expect("host address [::1]");
    tokio::spawn(host::serve(
        host.clone(),
        access,
        host_config(game_port, Vec::new()),
        metrics::Telemetry::new(),
    ));
    let cable = Cable::new(host_socket).await;

    // Ticket: a direct address (through the cable) + our own relay.
    let ticket = EndpointAddr::from_parts(
        host_key.public(),
        [
            TransportAddr::Ip(cable.addr),
            TransportAddr::Relay(url.clone()),
        ],
    );
    let guest_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(guest_key)
        .transport_config(endpoint::transport_config())
        .relay_mode(RelaySettings::default().with_relays_of(&ticket).mode())
        .clear_ip_transports()
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let handle = guest::start(
        guest_ep,
        guest_config(ticket, VoicePorts::Off),
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();
    let mut game = TcpStream::connect(handle.local_addr).await.unwrap();
    roundtrip(&mut game, b"hello", Duration::from_secs(5)).await;

    // At first data goes direct.
    wait_for_path(&handle, TransportAddr::is_ip, Duration::from_secs(15)).await;
    roundtrip(&mut game, b"direct", Duration::from_secs(5)).await;

    // The direct path is gone (Wi-Fi dropped, NAT forgot the connection).
    let relayed_before = relay.relayed_packets();
    cable.set_cut(true);
    let cut_at = Instant::now();
    game.write_all(b"after-cut").await.unwrap();
    read_exactly(&mut game, b"after-cut", Duration::from_secs(14)).await;
    let gap = cut_at.elapsed();
    eprintln!("pause when switching to the relay: {gap:?}");
    assert!(
        gap < Duration::from_secs(12),
        "switching to the relay took {gap:?}: the game may disconnect (it waits ~15 s)"
    );
    assert!(
        *handle.online.borrow(),
        "the connection to the host was not lost"
    );
    assert!(
        relay.relayed_packets() > relayed_before,
        "data went through the relay"
    );
    wait_for_path(&handle, TransportAddr::is_relay, Duration::from_secs(5)).await;
    roundtrip(&mut game, b"via-relay", Duration::from_secs(5)).await;

    // Connectivity is back: the game keeps working.
    cable.set_cut(false);
    roundtrip(&mut game, b"restored", Duration::from_secs(5)).await;
}

/// Our own relay was restarted (update, VPS reboot) during play through the relay:
/// the game connection does not break, data arrives once the relay works again.
#[tokio::test]
async fn game_survives_relay_restart() {
    let relay = plain_relay(AccessRules::default()).await;
    let relay_addr = relay.http_addr().unwrap();
    let url = relay.local_url("");
    let game_port = spawn_fake_game().await;
    let (host_key, guest_key) = (SecretKey::generate(), SecretKey::generate());
    let (_host, addr) = start_host(
        host_key,
        vec![guest_key.public()],
        game_port,
        Vec::new(),
        own_relay(&url, None),
    )
    .await;
    let ep = relay_only_endpoint(guest_key, RelaySettings::default().with_relays_of(&addr)).await;
    let handle = guest::start(
        ep,
        guest_config(addr, VoicePorts::Off),
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();
    let mut game = TcpStream::connect(handle.local_addr).await.unwrap();
    roundtrip(&mut game, b"before", Duration::from_secs(5)).await;

    relay.shutdown().await.unwrap();
    game.write_all(b"while-relay-restarts").await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let _relay = RelayServer::spawn(RelayOptions::plain_http(relay_addr))
        .await
        .unwrap();
    let restarted = Instant::now();

    read_exactly(&mut game, b"while-relay-restarts", Duration::from_secs(25)).await;
    eprintln!(
        "data arrived {:?} after the relay restart",
        restarted.elapsed()
    );
    assert!(
        *handle.online.borrow(),
        "the connection to the host was not lost"
    );
    roundtrip(&mut game, b"after", Duration::from_secs(5)).await;
}
