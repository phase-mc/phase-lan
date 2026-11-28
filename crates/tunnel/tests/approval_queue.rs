//! Several new players at once: the host is asked about them one at a time, in arrival order, and
//! the next question comes right after an answer. A guest who cancels leaves the line, and only a
//! full line turns a guest away. Everything on 127.0.0.1 with real connections.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use phase_tunnel::{
    EndpointAddr, EndpointId, SecretKey,
    access::{AccessHook, AccessList, Approver},
    endpoint::{self, EndpointOptions, NetworkMode},
    events::{AccessDecision, EventSink, TunnelEvent},
    guest::{self, NotAllowed},
    host::{self, HostConfig},
    metrics::Telemetry,
    wire::{HostInfo, RejectReason},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

const WAIT: Duration = Duration::from_secs(10);

/// A "let in?" question on the host's screen; the test answers through `answer`.
struct Question {
    id: EndpointId,
    nickname: Option<String>,
    answer: oneshot::Sender<bool>,
}

/// The host at the keyboard: every question goes to the test, which answers when it decides.
#[derive(Debug)]
struct Desk(mpsc::UnboundedSender<Question>);

impl Approver for Desk {
    fn approve(
        &self,
        id: EndpointId,
        nickname: Option<String>,
    ) -> n0_future::boxed::BoxFuture<bool> {
        let (answer, reply) = oneshot::channel();
        let _ = self.0.send(Question {
            id,
            nickname,
            answer,
        });
        Box::pin(async move { reply.await.unwrap_or(false) })
    }
}

struct Host {
    addr: EndpointAddr,
    events: Arc<Mutex<Vec<TunnelEvent>>>,
    questions: mpsc::UnboundedReceiver<Question>,
}

impl Host {
    async fn start(queue_limit: usize) -> Self {
        let (desk, questions) = mpsc::unbounded_channel();
        let events: Arc<Mutex<Vec<TunnelEvent>>> = Arc::default();
        let sink = EventSink::new({
            let events = events.clone();
            move |event| events.lock().unwrap().push(event)
        });
        let access = AccessList::in_memory([]);
        let hook = AccessHook::new(access.clone(), Some(Arc::new(Desk(desk))))
            .with_events(sink.clone())
            .with_queue_limit(queue_limit);
        let ep = endpoint::bind(EndpointOptions {
            secret_key: SecretKey::generate(),
            accept_incoming: true,
            network: NetworkMode::LoopbackOnly,
            relay: Default::default(),
            access: Some(hook),
        })
        .await
        .unwrap();
        let addr = endpoint::shareable_addr(&ep, NetworkMode::LoopbackOnly, Duration::ZERO).await;
        let cfg = HostConfig {
            game_port: Default::default(),
            motd: "World".into(),
            nickname: Some("Host".into()),
            udp_ports: Vec::new(),
            game_version: None,
            check_voice_owner: true,
            status_every: Duration::from_secs(60),
            mods: Default::default(),
            room: Default::default(),
        };
        tokio::spawn(host::serve(ep, access, cfg, Telemetry::with_events(sink)));
        Self {
            addr,
            events,
            questions,
        }
    }

    /// The next question on screen (fails if none comes).
    async fn next_question(&mut self) -> Question {
        tokio::time::timeout(WAIT, self.questions.recv())
            .await
            .expect("no question came")
            .expect("the host stopped asking")
    }

    /// Nobody else is being asked right now.
    fn no_other_question(&mut self) {
        if let Ok(q) = self.questions.try_recv() {
            panic!(
                "a second question while the first is on screen: {:?}",
                q.nickname
            );
        }
    }

    /// The line holds this many devices behind the question on screen.
    async fn line_is(&self, waiting: u32) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            if self.queue_reports().last() == Some(&waiting) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the line never reached {waiting}: {:?}",
                self.queue_reports()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn queue_reports(&self) -> Vec<u32> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                TunnelEvent::Queue { waiting } => Some(*waiting),
                _ => None,
            })
            .collect()
    }

    async fn decision_about(&self, id: EndpointId) -> AccessDecision {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let found = self.events.lock().unwrap().iter().find_map(|e| match e {
                TunnelEvent::Access { peer, decision, .. } if *peer == id.to_string() => {
                    Some(*decision)
                }
                _ => None,
            });
            if let Some(decision) = found {
                return decision;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "no decision about {id}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// A guest knocking under a nickname: its own device and endpoint, the connection runs in a task.
struct Guest {
    id: EndpointId,
    ep: iroh::Endpoint,
    knock: JoinHandle<anyhow::Result<HostInfo>>,
}

impl Guest {
    async fn knock(host: &Host, nickname: &str) -> Self {
        let key = SecretKey::generate();
        let id = key.public();
        let ep = endpoint::bind(EndpointOptions {
            secret_key: key,
            accept_incoming: false,
            network: NetworkMode::LoopbackOnly,
            relay: Default::default(),
            access: None,
        })
        .await
        .unwrap();
        let knock = tokio::spawn({
            let (ep, addr, nickname) = (ep.clone(), host.addr.clone(), nickname.to_string());
            async move {
                let (conn, info) = guest::connect_once(&ep, &addr, Some(&nickname)).await?;
                // Keep the connection open until the test ends.
                std::mem::forget(conn);
                Ok(info)
            }
        });
        Self { id, ep, knock }
    }

    async fn outcome(self) -> anyhow::Result<HostInfo> {
        tokio::time::timeout(WAIT, self.knock)
            .await
            .expect("the guest is still waiting")
            .unwrap()
    }

    async fn rejected_because(self) -> Option<RejectReason> {
        let err = self.outcome().await.expect_err("the guest got in");
        err.downcast_ref::<NotAllowed>()
            .unwrap_or_else(|| panic!("not a rejection: {err:#}"))
            .reason
    }
}

#[tokio::test]
async fn players_are_asked_one_at_a_time_in_arrival_order() {
    let mut host = Host::start(8).await;

    let steve = Guest::knock(&host, "Steve").await;
    let first = host.next_question().await;
    assert_eq!(first.id, steve.id);
    assert_eq!(first.nickname.as_deref(), Some("Steve"));

    // Two more knock while the host is looking at Steve: they wait, nobody else is asked.
    let alex = Guest::knock(&host, "Alex").await;
    host.line_is(1).await;
    let notch = Guest::knock(&host, "Notch").await;
    host.line_is(2).await;
    host.no_other_question();

    // Each answer brings the next question at once, in the order they came.
    first.answer.send(true).unwrap();
    let second = host.next_question().await;
    assert_eq!(second.id, alex.id);
    assert_eq!(second.nickname.as_deref(), Some("Alex"));
    host.line_is(1).await;
    host.no_other_question();

    second.answer.send(false).unwrap();
    let third = host.next_question().await;
    assert_eq!(third.id, notch.id);
    host.line_is(0).await;
    third.answer.send(true).unwrap();

    assert_eq!(steve.outcome().await.unwrap().motd, "World");
    assert_eq!(alex.rejected_because().await, Some(RejectReason::Denied));
    assert_eq!(notch.outcome().await.unwrap().motd, "World");
    assert_eq!(host.queue_reports(), vec![1, 2, 1, 0]);
}

#[tokio::test]
async fn a_player_who_cancels_leaves_the_line_and_is_never_asked() {
    let mut host = Host::start(8).await;
    let steve = Guest::knock(&host, "Steve").await;
    let first = host.next_question().await;
    let alex = Guest::knock(&host, "Alex").await;
    host.line_is(1).await;
    let notch = Guest::knock(&host, "Notch").await;
    host.line_is(2).await;

    // Alex gives up while waiting (the Cancel button closes the connection).
    alex.knock.abort();
    alex.ep.close().await;
    assert_eq!(
        host.decision_about(alex.id).await,
        AccessDecision::GuestLeft
    );
    host.line_is(1).await;

    // After Steve the host is asked about Notch, not about Alex.
    first.answer.send(true).unwrap();
    let second = host.next_question().await;
    assert_eq!(second.id, notch.id);
    second.answer.send(true).unwrap();
    host.no_other_question();

    assert!(steve.outcome().await.is_ok());
    assert!(notch.outcome().await.is_ok());
}

#[tokio::test]
async fn only_a_full_line_turns_a_player_away() {
    // One question on screen and room for one more in line.
    let mut host = Host::start(1).await;
    let steve = Guest::knock(&host, "Steve").await;
    let first = host.next_question().await;
    let alex = Guest::knock(&host, "Alex").await;
    host.line_is(1).await;

    let notch = Guest::knock(&host, "Notch").await;
    let notch_id = notch.id;
    assert_eq!(notch.rejected_because().await, Some(RejectReason::Busy));
    assert_eq!(host.decision_about(notch_id).await, AccessDecision::Busy);
    let text = RejectReason::Busy.explain();
    assert!(text.contains("очеред") || text.contains("line"), "{text}");

    // The ones in line are unaffected.
    first.answer.send(true).unwrap();
    let second = host.next_question().await;
    assert_eq!(second.id, alex.id);
    second.answer.send(true).unwrap();
    assert!(steve.outcome().await.is_ok());
    assert!(alex.outcome().await.is_ok());
}
