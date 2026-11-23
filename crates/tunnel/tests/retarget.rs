//! The host left the world and opened it to LAN again, and the game took a new port. The host
//! program changes the game port without closing the room: the same guest, without reconnecting,
//! reaches the world on the new port with its next game connection.

use std::time::Duration;

use phase_tunnel::{
    SecretKey,
    access::{AccessHook, AccessList},
    endpoint::{self, EndpointOptions, NetworkMode},
    guest::{self, GuestConfig},
    host::{self, GameTarget, HostConfig},
    metrics,
    voice::VoicePorts,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

const STATUS: Duration = Duration::from_secs(60);

/// A "world" instead of Minecraft: names itself to every connection and closes it.
async fn spawn_world(name: &'static [u8]) -> (u16, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = sock.write_all(name).await;
            let _ = sock.shutdown().await;
        }
    });
    (port, task)
}

/// Connects to the guest like a game and reads which world answered.
async fn which_world(guest_port: u16) -> Vec<u8> {
    let mut sock = TcpStream::connect(("127.0.0.1", guest_port)).await.unwrap();
    // The game has nothing to say: close our side and read the world's answer to the end.
    sock.shutdown().await.unwrap();
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), sock.read_to_end(&mut answer))
        .await
        .expect("the world did not answer");
    answer
}

#[tokio::test]
async fn guest_reaches_the_world_on_its_new_port() {
    let (old_port, old_world) = spawn_world(b"old").await;
    let guest_key = SecretKey::generate();
    let access = AccessList::in_memory(vec![guest_key.public()]);
    let host_ep = endpoint::bind(EndpointOptions {
        secret_key: SecretKey::generate(),
        accept_incoming: true,
        network: NetworkMode::LoopbackOnly,
        relay: Default::default(),
        access: Some(AccessHook::new(access.clone(), None)),
    })
    .await
    .unwrap();
    let host_addr =
        endpoint::shareable_addr(&host_ep, NetworkMode::LoopbackOnly, Duration::ZERO).await;
    let target = GameTarget::new(Some(old_port));
    let cfg = HostConfig {
        game_port: target.clone(),
        motd: "Тестовый мир".into(),
        nickname: None,
        udp_ports: Vec::new(),
        game_version: None,
        check_voice_owner: true,
        status_every: STATUS,
        mods: Default::default(),
        room: Default::default(),
    };
    tokio::spawn(host::serve(
        host_ep.clone(),
        access,
        cfg,
        metrics::Telemetry::new(),
    ));

    let guest_ep = endpoint::bind(EndpointOptions {
        secret_key: guest_key,
        accept_incoming: false,
        network: NetworkMode::LoopbackOnly,
        relay: Default::default(),
        access: None,
    })
    .await
    .unwrap();
    let handle = guest::start(
        guest_ep,
        GuestConfig {
            host: host_addr,
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
    assert_eq!(handle.host_info.game_port, Some(old_port));
    let guest_port = handle.local_addr.port();
    assert_eq!(which_world(guest_port).await, b"old");

    // The host left the world: the old port is closed.
    old_world.abort();
    let _ = old_world.await;
    assert!(
        which_world(guest_port).await.is_empty(),
        "there is no world on the old port anymore"
    );

    // The world is open to LAN again on a new port: the room moves there.
    let (new_port, _new_world) = spawn_world(b"new").await;
    assert_ne!(new_port, old_port);
    target.set(new_port);
    assert_eq!(target.get(), Some(new_port));
    assert_eq!(
        which_world(guest_port).await,
        b"new",
        "the same guest reaches the world on the new port without reconnecting"
    );
}

#[test]
fn empty_target_means_no_game() {
    assert_eq!(GameTarget::default().get(), None);
    assert_eq!(GameTarget::new(None).get(), None);
    let target = GameTarget::from(25565);
    let copy = target.clone();
    copy.set(51234);
    assert_eq!(target.get(), Some(51234), "clones share one port");
}
