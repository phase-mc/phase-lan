//! Connection drops and reconnection (test T6, automated).
//!
//! A UDP middlebox sits between the guest and the host: a "cable" the test can
//! cut (packets are dropped both ways) and switch to another host
//! (the host restarted the program). Everything on 127.0.0.1, without internet.
//! The "direct path ↔ relay" switch is tested in `relay.rs` (it has its own relay).

mod common;

use std::{net::SocketAddr, sync::atomic::Ordering, time::Duration};

use common::{Cable, read_exactly, roundtrip, spawn_fake_game, spawn_fake_voice, voice_roundtrip};
use iroh::TransportAddr;
use phase_tunnel::{
    EndpointAddr, SecretKey,
    access::AccessList,
    endpoint::{self, EndpointOptions, NetworkMode},
    guest::{self, GuestConfig},
    host::{self, HostConfig},
    metrics,
    voice::VoicePorts,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
};

const STATUS: Duration = Duration::from_secs(60);

/// A host with the given key. Returns the endpoint and its real address (UDP socket).
///
/// The host listens only on `[::1]` (IPv6), and the guest only on `127.0.0.1` (IPv4): all traffic
/// goes through the middlebox, and "cutting" it is real (see [`Cable`]).
/// Connection settings are the same as in the program (`endpoint::transport_config`).
async fn start_host(
    key: SecretKey,
    guest: phase_tunnel::EndpointId,
    game_port: u16,
    udp_ports: Vec<u16>,
) -> (iroh::Endpoint, SocketAddr) {
    let access = AccessList::in_memory([guest]);
    let ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(key)
        .transport_config(endpoint::transport_config())
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_ip_transports()
        .bind_addr("[::1]:0")
        .unwrap()
        .alpns(vec![phase_tunnel::wire::ALPN.to_vec()])
        .hooks(phase_tunnel::access::AccessHook::new(access.clone(), None))
        .bind()
        .await
        .unwrap();
    let socket = *ep
        .bound_sockets()
        .iter()
        .find(|a| a.is_ipv6())
        .expect("host address [::1]");
    let cfg = HostConfig {
        game_port: Some(game_port).into(),
        motd: "Мир".into(),
        nickname: None,
        udp_ports,
        game_version: None,
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
    (ep, socket)
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

/// A ticket "through the cable": the guest knows only the middlebox address.
fn via_cable(host_key: &SecretKey, cable: &Cable) -> EndpointAddr {
    EndpointAddr::from_parts(host_key.public(), [TransportAddr::Ip(cable.addr)])
}

/// A connection drop for `secs` seconds mid-game: data sent during the drop
/// arrives after recovery, and the game connection itself does not break.
async fn survive_outage(
    cable: &Cable,
    game: &mut TcpStream,
    online: &tokio::sync::watch::Receiver<bool>,
    secs: u64,
) {
    let dropped_before = cable.dropped.load(Ordering::SeqCst);
    cable.set_cut(true);
    game.write_all(b"during").await.unwrap();
    // The drop is real: no answer arrives while the cable is cut.
    let mut probe = [0u8; 6];
    let early = tokio::time::timeout(Duration::from_secs(2), game.read_exact(&mut probe)).await;
    assert!(early.is_err(), "data must not pass through a cut cable");
    tokio::time::sleep(Duration::from_secs(secs - 2)).await;
    assert!(cable.dropped.load(Ordering::SeqCst) > dropped_before);
    assert!(
        *online.borrow(),
        "{secs} s of outage: the connection must not count as lost yet"
    );
    cable.set_cut(false);

    read_exactly(game, b"during", Duration::from_secs(15)).await;
    roundtrip(game, b"after", Duration::from_secs(5)).await;
}

/// Drops of 5 and 10 s in a row on the **only** path (the plan's goal, test T6): connectivity
/// disappears completely, the game must not disconnect (it waits ~15 s).
///
/// There is one path here, and the QUIC library (noq) does not close the last path on idle,
/// so the connection just waits for connectivity to return.
#[tokio::test]
async fn game_connection_survives_5s_and_10s_outages() {
    let game_port = spawn_fake_game().await;
    let (host_key, guest_key) = (SecretKey::generate(), SecretKey::generate());
    let (_host, host_socket) =
        start_host(host_key.clone(), guest_key.public(), game_port, Vec::new()).await;
    let cable = Cable::new(host_socket).await;

    let handle = guest::start(
        guest_endpoint(guest_key).await,
        GuestConfig {
            host: via_cable(&host_key, &cable),
            nickname: None,
            entitlement: None,
            local_port: None,
            voice: VoicePorts::Off,
            status_every: STATUS,
        },
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();
    let mut game = TcpStream::connect(handle.local_addr).await.unwrap();
    roundtrip(&mut game, b"before", Duration::from_secs(5)).await;

    survive_outage(&cable, &mut game, &handle.online, 5).await;
    survive_outage(&cable, &mut game, &handle.online, 10).await;
}

/// The host restarted the program (same key, new network port): the guest reconnects
/// by itself, the old game connection closes right away (instead of hanging),
/// a new game connection and voice work again, without restarting the guest.
#[tokio::test]
async fn guest_reconnects_after_host_restart() {
    let game_port = spawn_fake_game().await;
    let voice_port = spawn_fake_voice().await;
    let (host_key, guest_key) = (SecretKey::generate(), SecretKey::generate());
    let (host1, socket1) = start_host(
        host_key.clone(),
        guest_key.public(),
        game_port,
        vec![voice_port],
    )
    .await;
    let cable = Cable::new(socket1).await;

    let handle = guest::start(
        guest_endpoint(guest_key.clone()).await,
        GuestConfig {
            host: via_cable(&host_key, &cable),
            nickname: None,
            entitlement: None,
            local_port: None,
            // Host and guest on one PC: the host's voice port is taken by the "server".
            voice: VoicePorts::Map(vec![(voice_port, 0)]),
            status_every: STATUS,
        },
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();
    let voice_local = handle.voice_bound[0].local;
    let voice_client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    voice_roundtrip(&voice_client, voice_local, b"voice-1").await;
    let mut old_game = TcpStream::connect(handle.local_addr).await.unwrap();
    roundtrip(&mut old_game, b"hello", Duration::from_secs(5)).await;

    // The host closes the program.
    host1.close().await;
    let mut online = handle.online.clone();
    tokio::time::timeout(Duration::from_secs(5), online.wait_for(|on| !*on))
        .await
        .expect("the guest should notice the host is gone")
        .unwrap();
    // The old game connection is closed right away instead of hanging as a "ghost".
    let mut buf = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(5), old_game.read(&mut buf))
        .await
        .expect("the old connection should close");
    assert!(matches!(closed, Ok(0) | Err(_)), "{closed:?}");

    // The host starts the program again, with the same key but on a new port.
    let (_host2, socket2) =
        start_host(host_key, guest_key.public(), game_port, vec![voice_port]).await;
    cable.switch_to(socket2);
    tokio::time::timeout(Duration::from_secs(20), online.wait_for(|on| *on))
        .await
        .expect("the guest should reconnect by itself")
        .unwrap();

    // The same local address: the game needs no changes, just "Join" again.
    let mut new_game = TcpStream::connect(handle.local_addr).await.unwrap();
    roundtrip(&mut new_game, b"again", Duration::from_secs(5)).await;
    voice_roundtrip(&voice_client, voice_local, b"voice-2").await;
}
