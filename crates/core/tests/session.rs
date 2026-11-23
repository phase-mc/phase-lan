//! End-to-end tests of the session layer (what the program window uses): host and guest on
//! one PC (`127.0.0.1`), the "let in?" question through the queue, a stop at any moment.

use std::{path::PathBuf, sync::Arc, time::Duration};

use phase_core::{
    ApprovalQueue, EventBus, GamePort, HostOptions, HostVoice, JoinOptions, NetOptions, Phase,
    Session, SessionEvent, SessionStatus,
};
use phase_minecraft::lan::AnnounceMode;
use phase_tunnel::{
    EndpointTicket,
    endpoint::NetworkMode,
    events::{AccessDecision, TunnelEvent},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::broadcast,
};

const WAIT: Duration = Duration::from_secs(30);

/// A separate temporary folder for each test (profiles, reports).
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "phase-core-test-{name}-{}-{}",
        std::process::id(),
        phase_tunnel::metrics::unix_now()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn loopback() -> NetOptions {
    NetOptions {
        mode: NetworkMode::LoopbackOnly,
        ..NetOptions::default()
    }
}

/// The "game": a TCP echo on 127.0.0.1.
async fn fake_game() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = tcp.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

fn host_options(dir: &std::path::Path, game_port: u16, approvals: &ApprovalQueue) -> HostOptions {
    let mut opts = HostOptions::new("host");
    opts.profile_dir = Some(dir.join("host"));
    opts.nickname = Some("HostNick".into());
    opts.game_port = Some(GamePort::Fixed(game_port));
    opts.voice = HostVoice::Off;
    opts.approver = Some(Arc::new(approvals.clone()));
    opts.net = loopback();
    opts.status_every = Duration::from_millis(300);
    opts.reports_dir = Some(dir.join("reports"));
    opts
}

fn join_options(dir: &std::path::Path, ticket: &str) -> JoinOptions {
    let ticket: EndpointTicket = ticket.parse().unwrap();
    let mut opts = JoinOptions::new("guest", ticket);
    opts.profile_dir = Some(dir.join("guest"));
    opts.nickname = Some("GrayIsko1337".into());
    opts.announce = AnnounceMode::Off;
    opts.voice = false;
    opts.net = loopback();
    opts.status_every = Duration::from_millis(300);
    opts
}

async fn wait_status(session: &Session, what: impl Fn(&SessionStatus) -> bool) -> SessionStatus {
    let mut rx = session.watch_status();
    let status = tokio::time::timeout(WAIT, rx.wait_for(|s| what(s) || s.phase.is_finished()))
        .await
        .expect("the session did not reach the needed state")
        .unwrap()
        .clone();
    assert!(what(&status), "the session finished earlier: {status:?}");
    status
}

/// Waits for an event matching `pick`, skipping the others.
async fn next_event<T>(
    events: &mut broadcast::Receiver<SessionEvent>,
    pick: impl Fn(&SessionEvent) -> Option<T>,
) -> T {
    tokio::time::timeout(WAIT, async {
        loop {
            let event = events.recv().await.expect("the event bus is closed");
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    })
    .await
    .expect("the event did not arrive")
}

#[tokio::test]
async fn host_and_guest_sessions_with_approval_and_clean_stop() {
    let dir = temp_dir("full");
    let game_port = fake_game().await;
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let approvals = ApprovalQueue::new(bus.clone());

    let host = Session::host(host_options(&dir, game_port, &approvals), bus.clone());
    let status = wait_status(&host, |s| s.phase == Phase::Running).await;
    let ready = status
        .host
        .expect("the host is ready but there is no ticket");
    assert_eq!(ready.game_port, Some(game_port));
    assert!(ready.relay.loopback_only);
    assert!(ready.asks_new_devices);
    assert!(status.device_id.is_some());
    assert_eq!(status.nickname.as_deref(), Some("HostNick"));
    assert!(ready.guests.is_empty());
    // A manually set port → there was a check that Minecraft is there (an echo is not Minecraft).
    let checked = next_event(&mut events, |e| match e {
        SessionEvent::GameChecked { port, error, .. } => Some((*port, error.is_some())),
        _ => None,
    })
    .await;
    assert_eq!(checked, (game_port, true));

    let mut guest = Session::join(join_options(&dir, &ready.ticket), bus.clone());

    // The host sees the "let in?" question with the guest's nickname and ID and answers "yes".
    let (request_id, device_id, nickname) = next_event(&mut events, |e| match e {
        SessionEvent::ApprovalRequested {
            request_id,
            device_id,
            nickname,
        } => Some((*request_id, device_id.clone(), nickname.clone())),
        _ => None,
    })
    .await;
    let waiting = wait_status(&guest, |s| s.device_id.is_some()).await;
    let guest_id = waiting.device_id.unwrap();
    assert_eq!(device_id, guest_id);
    assert_eq!(nickname.as_deref(), Some("GrayIsko1337"));
    // While waiting for the answer, the guest knows which nickname they are knocking under.
    assert_eq!(waiting.nickname.as_deref(), Some("GrayIsko1337"));
    let pending = approvals.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].nickname.as_deref(), Some("GrayIsko1337"));
    assert!(approvals.answer(request_id, true));

    let status = wait_status(&guest, |s| s.phase == Phase::Running).await;
    let joined = status.guest.unwrap();
    assert!(joined.lan_name.is_none(), "announcement is off");
    // The guest sees the host's nickname, the host sees the guest in the "who is in the room" list.
    assert_eq!(joined.host_nickname.as_deref(), Some("HostNick"));
    let room = wait_status(&host, |s| {
        s.host.as_ref().is_some_and(|h| !h.guests.is_empty())
    })
    .await
    .host
    .unwrap();
    assert_eq!(room.guests.len(), 1);
    assert_eq!(room.guests[0].device_id, guest_id);
    assert_eq!(room.guests[0].nickname.as_deref(), Some("GrayIsko1337"));
    assert_eq!(room.guests[0].connections, 1);

    // The guest's game → tunnel → the host's "game" and back.
    let mut tcp = TcpStream::connect(&joined.local_addr).await.unwrap();
    tcp.write_all(b"LanBridge").await.unwrap();
    let mut buf = [0u8; 9];
    tokio::time::timeout(WAIT, tcp.read_exact(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf, b"LanBridge");

    // Core events reached the bus: access decision, guest connection, ping.
    next_event(&mut events, |e| {
        matches!(
            e,
            SessionEvent::Tunnel(TunnelEvent::Access {
                decision: AccessDecision::Allowed,
                ..
            })
        )
        .then_some(())
    })
    .await;
    next_event(&mut events, |e| {
        matches!(e, SessionEvent::Tunnel(TunnelEvent::Ping { .. })).then_some(())
    })
    .await;

    // The guest stops: the connection is closed, the game port is no longer open.
    let local_addr = joined.local_addr.clone();
    guest.stop();
    guest.wait().await.unwrap();
    assert_eq!(guest.status().phase, Phase::Stopped);
    assert!(
        TcpStream::connect(&local_addr).await.is_err(),
        "after the stop the guest's port must be closed"
    );
    next_event(&mut events, |e| {
        matches!(
            e,
            SessionEvent::Tunnel(TunnelEvent::PeerDisconnected { .. })
        )
        .then_some(())
    })
    .await;

    // The guest left: nobody is in the room.
    wait_status(&host, |s| {
        s.host.as_ref().is_some_and(|h| h.guests.is_empty())
    })
    .await;

    host.shutdown().await.unwrap();
    let report = next_event(&mut events, |e| match e {
        SessionEvent::ReportSaved { path } => Some(PathBuf::from(path)),
        _ => None,
    })
    .await;
    assert!(report.exists(), "the host's report is saved");
    // The approved device is written to the host's profile together with the friend's nickname.
    let allowed = std::fs::read_to_string(dir.join("host").join("allowed.txt")).unwrap();
    assert!(allowed.contains(&guest_id));
    assert!(
        allowed.contains(&format!("#@ {guest_id} GrayIsko1337")),
        "{allowed}"
    );
    // The session nickname is set explicitly: it is not written to the profile.
    assert!(!dir.join("host").join("nickname.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn stop_while_waiting_for_approval() {
    let dir = temp_dir("stop");
    let game_port = fake_game().await;
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let approvals = ApprovalQueue::new(bus.clone());

    let host = Session::host(host_options(&dir, game_port, &approvals), bus.clone());
    let ticket = wait_status(&host, |s| s.host.is_some())
        .await
        .host
        .unwrap()
        .ticket;
    let guest = Session::join(join_options(&dir, &ticket), bus.clone());
    let request_id = next_event(&mut events, |e| match e {
        SessionEvent::ApprovalRequested { request_id, .. } => Some(*request_id),
        _ => None,
    })
    .await;
    assert_eq!(guest.status().phase, Phase::Connecting);

    // The guest changed their mind while the host is deciding: a quick stop without an error…
    tokio::time::timeout(Duration::from_secs(10), guest.shutdown())
        .await
        .expect("the guest's stop hung")
        .unwrap();
    // …and the host's question closes itself: there is no need to answer.
    next_event(&mut events, |e| {
        matches!(e, SessionEvent::ApprovalClosed { request_id: id } if *id == request_id)
            .then_some(())
    })
    .await;
    assert!(!approvals.answer(request_id, true));

    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The host thought and answered "no": the guest's session ends at once with a clear reason,
/// and does not hang in "connecting".
#[tokio::test]
async fn guest_session_fails_right_after_host_says_no() {
    let dir = temp_dir("deny");
    let game_port = fake_game().await;
    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let approvals = ApprovalQueue::new(bus.clone());

    let host = Session::host(host_options(&dir, game_port, &approvals), bus.clone());
    let ticket = wait_status(&host, |s| s.host.is_some())
        .await
        .host
        .unwrap()
        .ticket;
    let mut guest = Session::join(join_options(&dir, &ticket), bus.clone());
    let request_id = next_event(&mut events, |e| match e {
        SessionEvent::ApprovalRequested { request_id, .. } => Some(*request_id),
        _ => None,
    })
    .await;

    // The host thinks for several seconds (like a person before the "Let in?" window) and refuses.
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(approvals.answer(request_id, false));

    let result = tokio::time::timeout(Duration::from_secs(5), guest.wait())
        .await
        .expect("the host answered no, but the guest's session did not finish in 5 s");
    let error = format!("{:#}", result.expect_err("the guest was not let in"));
    assert!(error.contains("хост ответил «нет»"), "{error}");
    let status = guest.status();
    assert_eq!(status.phase, Phase::Failed);
    assert!(status.guest.is_none());
    next_event(&mut events, |e| {
        matches!(
            e,
            SessionEvent::Tunnel(TunnelEvent::Access {
                decision: AccessDecision::Denied,
                ..
            })
        )
        .then_some(())
    })
    .await;

    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn bad_settings_fail_with_a_clear_error() {
    let dir = temp_dir("fail");
    let bus = EventBus::new();
    let mut opts = HostOptions::new("host");
    opts.profile_dir = Some(dir.join("host"));
    opts.net.relay_ca = Some(dir.join("нет-такого-файла.pem"));
    let mut host = Session::host(opts, bus);
    assert!(host.wait().await.is_err());
    let status = host.status();
    assert_eq!(status.phase, Phase::Failed);
    assert!(status.error.unwrap().contains("нет-такого-файла"));
    let _ = std::fs::remove_dir_all(&dir);
}
