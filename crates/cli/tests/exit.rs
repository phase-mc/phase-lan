//! The console program must exit by itself when the session has ended.
//!
//! Checked by running the real `phase-cli.exe`: the bug was in the program's output loop
//! (it spun idle after the session ended), not in the core.

use std::{
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

/// Runs the program with its own data folder and waits for it at most `limit`.
/// `None` means the program did not exit by itself (and was killed).
fn run(args: &[&str], data: &PathBuf, limit: Duration) -> Option<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_phase-cli"))
        .args(args)
        // Test profiles and keys go to a temp folder, not the user's folder.
        .env("LOCALAPPDATA", data)
        // Texts are checked in Russian, whatever the Windows language.
        .env("PHASE_LANG", "ru")
        .current_dir(data)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start phase-cli");
    let started = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return Some(child.wait_with_output().unwrap());
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phase-cli-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn exits_with_an_error_when_the_session_fails() {
    let data = temp_dir("fail");
    // There is no certificate file, so the host session ends with an error right away.
    let output = run(
        &["host", "--no-prompt", "--relay-ca", "no-such-file.pem"],
        &data,
        Duration::from_secs(20),
    )
    .expect("the session ended with an error but the program did not exit (hung)");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no-such-file.pem"), "{stderr}");
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn id_prints_the_device_id_and_exits() {
    let data = temp_dir("id");
    let output = run(&["id"], &data, Duration::from_secs(20)).expect("the id command hung");
    assert!(output.status.success());
    let id = String::from_utf8_lossy(&output.stdout);
    assert_eq!(id.trim().len(), 64, "ID is 64 hex characters: {id}");
    let _ = std::fs::remove_dir_all(&data);
}
