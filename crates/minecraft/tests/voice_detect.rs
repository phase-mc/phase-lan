//! Search for voice ports on a "fake game" inside the test process: the world port (TCP),
//! a voice port and one more UDP port, plus a game log with voice mod lines.
//!
//! The "game process" here is the test process itself, so the tests of this file run
//! one at a time (a shared lock): otherwise sockets of neighboring tests would get into the "game's"
//! port table. Logs are read only from the test's temporary folder, not from launcher folders.
//! Everything opens on 127.0.0.1 only.

use std::{
    fs,
    net::{Ipv4Addr, TcpListener, UdpSocket},
    path::PathBuf,
    sync::Mutex,
};

use phase_minecraft::voice::{self, PortSource, VoiceMod};

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn udp() -> (UdpSocket, u16) {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = s.local_addr().unwrap().port();
    (s, port)
}

fn closed_udp_port() -> u16 {
    udp().1
}

/// Temporary "game" folder with a `logs\latest.log` log.
fn game_dir(name: &str, log: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phase-test-voice-{name}-{}", std::process::id()));
    let logs = dir.join("logs");
    fs::create_dir_all(&logs).unwrap();
    fs::write(logs.join("latest.log"), log).unwrap();
    dir
}

/// The fallback port 24454 may be taken by a real program on the developer's PC,
/// and then it honestly lands in the "skipped" list. We check that it is accounted for one way or the other.
fn fallback_handled(found: &voice::VoiceDetection, port: u16) -> bool {
    found.ports.iter().any(|p| p.port == port) || found.skipped.iter().any(|(p, _)| *p == port)
}

#[cfg(windows)]
#[test]
fn log_port_of_verified_game_comes_first() {
    let _lock = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let game = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let game_port = game.local_addr().unwrap().port();
    let (_svc, svc_port) = udp();
    let (_other, other_port) = udp();
    let stale_port = closed_udp_port(); // from the log of a previous launch, closed now

    let dir = game_dir(
        "log",
        &format!(
            "[10:00:00] [Server thread/INFO]: Voice chat server started at port {stale_port}\n\
             [10:05:00] [Server thread/INFO]: Starting integrated minecraft server\n\
             [10:05:01] [VoiceChatServerThread/INFO]: [voicechat] Voice chat server started at port {svc_port}\n"
        ),
    );
    let logs = voice::latest_logs(std::slice::from_ref(&dir), &[]);
    assert_eq!(logs, vec![dir.join("logs").join("latest.log")]);

    let found = voice::detect(
        game_port,
        true,
        &logs,
        &[game_port, voice::SVC_DEFAULT_PORT],
    );
    assert_eq!(found.game_pid, Some(std::process::id()));
    let ports = found.port_numbers();

    // First comes the port from the log, confirmed by the game process's port table.
    assert_eq!(ports[0], svc_port, "{found:?}");
    assert!(matches!(
        &found.ports[0].source,
        PortSource::Log {
            voice_mod: VoiceMod::SimpleVoiceChat,
            ..
        }
    ));
    assert!(!ports.contains(&stale_port), "old log entry: {ports:?}");
    // Fallback ports are accounted for (announced or, if a foreign program holds them, skipped).
    assert!(fallback_handled(&found, game_port));
    assert!(fallback_handled(&found, voice::SVC_DEFAULT_PORT));
    // The log found voice: the game's other sockets are not announced.
    assert!(!ports.contains(&other_port), "{found:?}");
    assert!(ports.len() <= voice::MAX_PORTS && !ports.contains(&4445));
    fs::remove_dir_all(&dir).unwrap();
}

#[cfg(windows)]
#[test]
fn without_log_other_game_sockets_are_offered() {
    let _lock = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let game = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let game_port = game.local_addr().unwrap().port();
    let (_a, a) = udp();
    let (_b, b) = udp();
    let found = voice::detect(game_port, true, &[], &[game_port]);
    for port in [a, b] {
        let entry = found.ports.iter().find(|p| p.port == port);
        assert_eq!(
            entry.map(|e| &e.source),
            Some(&PortSource::UnidentifiedGameSocket),
            "{found:?}"
        );
    }
}

#[cfg(windows)]
#[test]
fn unverified_port_trusts_nothing() {
    let _lock = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // The port is listened on by "some program" (the Minecraft status is not confirmed). The log names
    // an open UDP port, and the same port is given as a fallback.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (_busy, busy_port) = udp();
    let dir = game_dir(
        "unverified",
        &format!(
            "[10:00:00] [Server thread/INFO]: Voice chat server started at port {busy_port}\n"
        ),
    );
    let logs = voice::latest_logs(std::slice::from_ref(&dir), &[]);

    let found = voice::detect(port, false, &logs, &[port, busy_port]);
    assert_eq!(
        found.game_pid, None,
        "without a status query the process does not count as the game"
    );
    assert!(
        found.logs_read.is_empty(),
        "we do not trust the log without a game process"
    );
    // A port taken by an unconfirmed process is not announced; only free
    // fallback ports remain.
    assert!(!found.port_numbers().contains(&busy_port));
    assert!(
        found
            .skipped
            .iter()
            .any(|(p, why)| *p == busy_port && why.contains("не определена"))
    );
    assert!(fallback_handled(&found, port));
    assert!(found.ports.iter().all(|p| p.source == PortSource::Fallback));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unknown_game_port_leaves_only_fallback() {
    let _lock = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    // A port nobody listens on: no game process found, no logs.
    let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // Not "confirmed": otherwise, if a foreign program takes the port right away, the test would take
    // it for the game.
    let found = voice::detect(free, false, &[], &[free, voice::SVC_DEFAULT_PORT]);
    assert_eq!(found.game_pid, None);
    assert!(fallback_handled(&found, free));
    assert!(fallback_handled(&found, voice::SVC_DEFAULT_PORT));
    assert!(found.ports.iter().all(|p| p.source == PortSource::Fallback));
}

#[test]
fn user_game_dir_is_read_even_if_old_and_comes_first() {
    let _lock = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let user = game_dir("user", "old log\n");
    let known = game_dir("known", "new log\n");
    // The log in the user's folder is "old" (two days), in the known one it is fresh.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(48 * 3600);
    fs::File::options()
        .write(true)
        .open(user.join("logs").join("latest.log"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    let missing = std::env::temp_dir().join("phase-test-voice-missing-dir");
    let logs = voice::latest_logs(&[user.clone(), missing], std::slice::from_ref(&known));
    assert_eq!(
        logs,
        vec![
            user.join("logs").join("latest.log"),
            known.join("logs").join("latest.log")
        ]
    );
    // An old log in a known folder is skipped.
    let logs = voice::latest_logs(&[], std::slice::from_ref(&user));
    assert!(logs.is_empty());
    fs::remove_dir_all(&user).unwrap();
    fs::remove_dir_all(&known).unwrap();
}
