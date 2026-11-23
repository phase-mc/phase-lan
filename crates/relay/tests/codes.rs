//! The room code service for real: HTTP on 127.0.0.1, our own relay next to it and the Phase
//! tunnel. The host registers a code, the guest types it and plays through the relay.

use std::{net::SocketAddr, str::FromStr, time::Duration};

use phase_codes::client::{CodesClient, CodesError};
use phase_relay::{CodesOptions, CodesServer, RelayOptions, RelayServer, codes::Limits};
use phase_tunnel::{
    EndpointTicket, SecretKey,
    access::{AccessHook, AccessList},
    endpoint::{self, EndpointOptions, NetworkMode, RelaySettings},
    guest::{self, GuestConfig},
    host::{self, HostConfig},
    metrics,
    voice::VoicePorts,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const ONLINE_WAIT: Duration = Duration::from_secs(10);
const STATUS: Duration = Duration::from_secs(60);

fn any_local() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

async fn codes_service(limits: Limits) -> CodesServer {
    let mut opts = CodesOptions::new(any_local());
    opts.limits = limits;
    CodesServer::spawn(opts).await.unwrap()
}

/// The "game": a TCP echo server.
async fn spawn_echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

fn relay_settings(url: &phase_tunnel::RelayUrl) -> RelaySettings {
    RelaySettings {
        urls: vec![url.clone()],
        ..Default::default()
    }
}

/// The host registers a code; the guest types it lowercase without the dash, gets the ticket and
/// plays through our relay. After the room closes, the code is no longer found.
#[tokio::test]
async fn guest_joins_by_code_through_own_relay() {
    let relay = RelayServer::spawn(RelayOptions::plain_http(any_local()))
        .await
        .unwrap();
    let url = relay.local_url("");
    let codes = codes_service(Limits::default()).await;
    let client = CodesClient::new(&codes.local_url()).unwrap();

    // Host: relay only, the guest is allowed in advance (the "let in?" question is not this test's topic).
    let game_port = spawn_echo().await;
    let (host_key, guest_key) = (SecretKey::generate(), SecretKey::generate());
    let access = AccessList::in_memory(vec![guest_key.public()]);
    let host_ep = endpoint::bind(EndpointOptions {
        secret_key: host_key,
        accept_incoming: true,
        network: NetworkMode::RelayOnly,
        relay: relay_settings(&url),
        access: Some(AccessHook::new(access.clone(), None)),
    })
    .await
    .unwrap();
    let addr = endpoint::shareable_addr(&host_ep, NetworkMode::RelayOnly, ONLINE_WAIT).await;
    tokio::spawn(host::serve(
        host_ep.clone(),
        access,
        HostConfig {
            game_port: Some(game_port).into(),
            motd: "Мир по коду".into(),
            nickname: None,
            udp_ports: vec![],
            game_version: None,
            check_voice_owner: true,
            status_every: STATUS,
            mods: Default::default(),
            room: Default::default(),
        },
        metrics::Telemetry::new(),
    ));
    let ticket = EndpointTicket::new(addr.clone()).to_string();
    let registered = client.register(&ticket).await.unwrap();
    assert!(phase_codes::is_code(&registered.code));
    client
        .renew(&registered.code, &registered.secret, &ticket)
        .await
        .unwrap();

    // Guest: the code as dictated, lowercase, without the dash.
    let typed = registered.code.to_lowercase().replace('-', "");
    let found = client.lookup(&typed).await.unwrap();
    assert_eq!(found, ticket);
    let host_addr = EndpointTicket::from_str(&found)
        .unwrap()
        .endpoint_addr()
        .clone();
    let guest_ep = endpoint::bind(EndpointOptions {
        secret_key: guest_key,
        accept_incoming: false,
        network: NetworkMode::RelayOnly,
        relay: RelaySettings::default().with_relays_of(&host_addr),
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
            voice: VoicePorts::default(),
            status_every: STATUS,
        },
        metrics::Telemetry::new(),
    )
    .await
    .unwrap();
    let mut game = TcpStream::connect(handle.local_addr).await.unwrap();
    game.write_all(b"joined-by-code").await.unwrap();
    let mut echo = [0u8; 14];
    tokio::time::timeout(Duration::from_secs(5), game.read_exact(&mut echo))
        .await
        .expect("game echo through the relay")
        .unwrap();
    assert_eq!(&echo, b"joined-by-code");

    // Room closed: the code is released.
    client
        .release(&registered.code, &registered.secret)
        .await
        .unwrap();
    assert!(matches!(
        client.lookup(&registered.code).await,
        Err(CodesError::NotFound)
    ));
}

/// Another host cannot renew or release a code; lookups from one IP are limited with `429`.
#[tokio::test]
async fn http_conflict_and_rate_limit() {
    let codes = codes_service(Limits {
        lookups_per_min: 6,
        lookup_burst: 3,
        ..Limits::default()
    })
    .await;
    let client = CodesClient::new(&codes.local_url()).unwrap();
    let ticket = "endpointaaaabbbbcccc";
    let registered = client.register(ticket).await.unwrap();

    let thief = "f".repeat(32);
    assert!(matches!(
        client.renew(&registered.code, &thief, "endpointzzzz").await,
        Err(CodesError::Conflict)
    ));
    assert!(matches!(
        client.release(&registered.code, &thief).await,
        Err(CodesError::Conflict)
    ));

    for _ in 0..3 {
        assert_eq!(client.lookup(&registered.code).await.unwrap(), ticket);
    }
    assert!(matches!(
        client.lookup(&registered.code).await,
        Err(CodesError::RateLimited)
    ));
}

/// Service not running: an "unavailable" error, the window offers the long invite.
#[tokio::test]
async fn service_down_is_reported_as_unavailable() {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let client = CodesClient::new(&format!("http://127.0.0.1:{port}")).unwrap();
    let err = client.lookup("K7F-2QX").await.unwrap_err();
    assert!(err.is_unavailable(), "{err:?}");
}
