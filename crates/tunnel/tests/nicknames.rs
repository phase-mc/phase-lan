//! Game nicknames in the handshake: the host sees the guest's nickname in the "let in?" question and in events,
//! the guest sees the host's nickname, an allowed friend's nickname is remembered. Everything on 127.0.0.1.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use phase_tunnel::{
    EndpointAddr, EndpointId, SecretKey,
    access::{AccessHook, AccessList, Approver},
    endpoint::{self, EndpointOptions, NetworkMode},
    events::{AccessDecision, EventSink, TunnelEvent},
    guest,
    host::{self, HostConfig},
    metrics::Telemetry,
};

const WAIT: Duration = Duration::from_secs(10);

/// The "let in?" question: who and under which nickname.
type Question = (EndpointId, Option<String>);

/// A "host" that says "yes" and records whom it was asked about and under which nickname.
#[derive(Debug, Clone, Default)]
struct Recorder {
    asked: Arc<Mutex<Vec<Question>>>,
}

impl Approver for Recorder {
    fn approve(
        &self,
        id: EndpointId,
        nickname: Option<String>,
    ) -> n0_future::boxed::BoxFuture<bool> {
        self.asked.lock().unwrap().push((id, nickname));
        Box::pin(async { true })
    }
}

struct Host {
    addr: EndpointAddr,
    access: AccessList,
    events: Arc<Mutex<Vec<TunnelEvent>>>,
}

impl Host {
    /// Waits for an event for which `pick` returned a value.
    async fn wait_for<T>(&self, what: &str, pick: impl Fn(&TunnelEvent) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if let Some(found) = self.events.lock().unwrap().iter().find_map(&pick) {
                return found;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no '{what}' event: {:?}",
                self.events.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn start_host(access: AccessList, approver: Option<Arc<dyn Approver>>) -> Host {
    let events: Arc<Mutex<Vec<TunnelEvent>>> = Arc::default();
    let sink = EventSink::new({
        let events = events.clone();
        move |event| events.lock().unwrap().push(event)
    });
    let ep = endpoint::bind(EndpointOptions {
        secret_key: SecretKey::generate(),
        accept_incoming: true,
        network: NetworkMode::LoopbackOnly,
        relay: Default::default(),
        access: Some(AccessHook::new(access.clone(), approver).with_events(sink.clone())),
    })
    .await
    .unwrap();
    let addr = endpoint::shareable_addr(&ep, NetworkMode::LoopbackOnly, Duration::ZERO).await;
    let cfg = HostConfig {
        game_port: Default::default(),
        motd: "Мир".into(),
        nickname: Some("HostNick".into()),
        udp_ports: Vec::new(),
        game_version: None,
        check_voice_owner: true,
        status_every: Duration::from_secs(60),
        mods: Default::default(),
        room: Default::default(),
    };
    tokio::spawn(host::serve(
        ep,
        access.clone(),
        cfg,
        Telemetry::with_events(sink),
    ));
    Host {
        addr,
        access,
        events,
    }
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
async fn host_sees_nickname_in_the_question_and_guest_sees_host_nickname() {
    let approver = Recorder::default();
    let host = start_host(AccessList::in_memory([]), Some(Arc::new(approver.clone()))).await;

    let key = SecretKey::generate();
    let id = key.public();
    let ep = guest_endpoint(key).await;
    // The nickname is sent sanitized: surrounding spaces and control characters are removed.
    let (_conn, info) = tokio::time::timeout(
        WAIT,
        guest::connect_once(&ep, &host.addr, Some("  GrayIsko\u{1b}1337 ")),
    )
    .await
    .expect("handshake hung: no reply to the hello read before the question")
    .unwrap();

    // The guest sees the host's nickname.
    assert_eq!(info.nickname.as_deref(), Some("HostNick"));
    // The host was asked once, already with the nickname.
    assert_eq!(
        *approver.asked.lock().unwrap(),
        vec![(id, Some("GrayIsko1337".to_string()))]
    );
    // The nickname is remembered next to the ID in the allow list.
    assert_eq!(host.access.nickname(&id).as_deref(), Some("GrayIsko1337"));

    let allowed = host
        .wait_for("access decision", |e| match e {
            TunnelEvent::Access {
                peer,
                nickname,
                decision: AccessDecision::Allowed,
            } if *peer == id.to_string() => Some(nickname.clone()),
            _ => None,
        })
        .await;
    assert_eq!(allowed.as_deref(), Some("GrayIsko1337"));
    let connected = host
        .wait_for("guest connected", |e| match e {
            TunnelEvent::PeerConnected { peer, nickname } if *peer == id.to_string() => {
                Some(nickname.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(connected.as_deref(), Some("GrayIsko1337"));
}

#[tokio::test]
async fn guest_of_older_version_without_nickname_still_gets_in() {
    let approver = Recorder::default();
    let host = start_host(AccessList::in_memory([]), Some(Arc::new(approver.clone()))).await;

    let key = SecretKey::generate();
    let id = key.public();
    let ep = guest_endpoint(key).await;
    // Without a nickname the hello is byte for byte the same as in versions before nicknames.
    let (_conn, info) = tokio::time::timeout(WAIT, guest::connect_once(&ep, &host.addr, None))
        .await
        .expect("handshake hung")
        .unwrap();
    assert_eq!(info.motd, "Мир");
    assert_eq!(*approver.asked.lock().unwrap(), vec![(id, None)]);
    assert!(host.access.is_allowed(&id));
    assert_eq!(host.access.nickname(&id), None);

    let connected = host
        .wait_for("guest connected", |e| match e {
            TunnelEvent::PeerConnected { peer, nickname } if *peer == id.to_string() => {
                Some(nickname.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(connected, None);
}

#[tokio::test]
async fn known_friend_is_shown_by_remembered_nickname_and_renames_are_picked_up() {
    let key = SecretKey::generate();
    let id = key.public();
    let access = AccessList::in_memory([]);
    access.allow_as(id, Some("OldNick")).unwrap();
    // Questions are off: the friend is already on the list, nobody to ask.
    let host = start_host(access, None).await;

    let ep = guest_endpoint(key).await;
    let (_conn, _info) =
        tokio::time::timeout(WAIT, guest::connect_once(&ep, &host.addr, Some("NewNick")))
            .await
            .expect("handshake hung")
            .unwrap();

    // Right on connecting, under the nickname remembered from last time…
    let connected = host
        .wait_for("guest connected", |e| match e {
            TunnelEvent::PeerConnected { peer, nickname } if *peer == id.to_string() => {
                Some(nickname.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(connected.as_deref(), Some("OldNick"));
    // …and after the hello, under the new one, which is remembered.
    let renamed = host
        .wait_for("guest introduced itself", |e| match e {
            TunnelEvent::PeerNickname { peer, nickname } if *peer == id.to_string() => {
                Some(nickname.clone())
            }
            _ => None,
        })
        .await;
    assert_eq!(renamed, "NewNick");
    assert_eq!(host.access.nickname(&id).as_deref(), Some("NewNick"));
}
