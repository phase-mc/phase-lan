//! Seats in the room: the host's "Seats in the room" setting, the game's own limit and the "room is
//! full" refusal. Everything is free, there are no plans in LAN. The host and guests are on one PC
//! (`127.0.0.1`), as in `session.rs`.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use n0_future::boxed::BoxFuture;
use phase_core::{
    EventBus, GamePort, HostOptions, HostVoice, JoinOptions, LookCommand, LookSettings, NetOptions,
    Phase, Session, SessionEvent, SessionStatus, look,
};
use phase_minecraft::lan::AnnounceMode;
use phase_tunnel::{
    EndpointId, EndpointTicket,
    access::Approver,
    endpoint::NetworkMode,
    events::{AccessDecision, TunnelEvent},
};
use tokio::net::TcpListener;

const WAIT: Duration = Duration::from_secs(30);

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "phase-rooms-test-{name}-{}-{}",
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

/// The "game": a port that simply accepts connections.
async fn fake_game() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            drop(tcp);
        }
    });
    port
}

/// A host who lets everyone in: the test is about seats, not about the "let in?" question.
#[derive(Debug)]
struct AllowAll;

impl Approver for AllowAll {
    fn approve(&self, _id: EndpointId, _nickname: Option<String>) -> BoxFuture<bool> {
        Box::pin(async { true })
    }
}

fn host_options(dir: &Path, game_port: u16) -> HostOptions {
    let mut opts = HostOptions::new("host");
    opts.profile_dir = Some(dir.join("host"));
    opts.nickname = Some("HostNick".into());
    opts.game_port = Some(GamePort::Fixed(game_port));
    opts.voice = HostVoice::Off;
    opts.approver = Some(Arc::new(AllowAll));
    opts.net = loopback();
    opts.status_every = Duration::from_millis(300);
    opts
}

fn join_options(dir: &Path, name: &str, ticket: &str) -> JoinOptions {
    let ticket: EndpointTicket = ticket.parse().unwrap();
    let mut opts = JoinOptions::new(name, ticket);
    opts.profile_dir = Some(dir.join(name));
    opts.nickname = Some(name.into());
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

async fn running(session: &Session) -> SessionStatus {
    wait_status(session, |s| s.phase == Phase::Running).await
}

#[tokio::test]
async fn without_companion_the_room_has_the_games_eight_places() {
    let dir = temp_dir("vanilla");
    let host = Session::host(host_options(&dir, fake_game().await), EventBus::new());
    let ready = running(&host).await.host.unwrap();
    // By default the host wants 20 seats, but without Phase Companion the game admits only 8.
    assert_eq!(look::load_settings(&dir.join("host")).slots, 20);
    assert_eq!(ready.room.max_players, 8);

    let guest = Session::join(join_options(&dir, "guest", &ready.ticket), EventBus::new());
    let room = running(&guest).await.guest.unwrap().room;
    assert_eq!(
        room.map(|r| r.max_players),
        Some(8),
        "the guest sees the same seats"
    );

    guest.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn host_slot_setting_limits_the_room_and_changes_on_the_fly() {
    let dir = temp_dir("slots");
    // The host decided: three in the room (including them).
    let settings = LookSettings {
        slots: 3,
        ..LookSettings::default()
    };
    look::save_settings(&dir.join("host"), &settings).unwrap();

    let bus = EventBus::new();
    let mut events = bus.subscribe();
    let host = Session::host(host_options(&dir, fake_game().await), bus.clone());
    let ready = running(&host).await.host.unwrap();
    assert_eq!(ready.room.max_players, 3);

    // The host and two guests: the room is full.
    let mut guests = Vec::new();
    for n in 1..=2 {
        let guest = Session::join(
            join_options(&dir, &format!("guest{n}"), &ready.ticket),
            EventBus::new(),
        );
        let room = running(&guest).await.guest.unwrap().room;
        assert_eq!(room.map(|r| r.max_players), Some(3));
        guests.push(guest);
    }
    wait_status(&host, |s| {
        s.host.as_ref().is_some_and(|h| h.guests.len() == 2)
    })
    .await;

    // The next player gets a clear refusal, the host gets a "room is full" entry.
    let mut late = Session::join(join_options(&dir, "late", &ready.ticket), EventBus::new());
    let err = tokio::time::timeout(WAIT, late.wait())
        .await
        .expect("the guest did not get a refusal")
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("комната заполнена: 3 из 3"),
        "{err:#}"
    );
    let decision = tokio::time::timeout(WAIT, async {
        loop {
            if let SessionEvent::Tunnel(TunnelEvent::Access { decision, .. }) =
                events.recv().await.unwrap()
                && decision != AccessDecision::Allowed
            {
                return decision;
            }
        }
    })
    .await
    .expect("the host did not report the refusal");
    assert_eq!(decision, AccessDecision::RoomFull);

    // The host added a seat right in the open room: the latecomer gets in.
    assert!(host.look_command(LookCommand::Settings(LookSettings {
        slots: 4,
        ..settings
    })));
    wait_status(&host, |s| {
        s.host.as_ref().is_some_and(|h| h.room.max_players == 4)
    })
    .await;
    let again = Session::join(join_options(&dir, "late", &ready.ticket), EventBus::new());
    running(&again).await;
    assert_eq!(look::load_settings(&dir.join("host")).slots, 4);

    again.shutdown().await.unwrap();
    for guest in guests {
        guest.shutdown().await.unwrap();
    }
    host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
