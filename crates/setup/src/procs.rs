//! Running Phase instances and the polite request to close.
//!
//! Processes are only looked up here: the installer can't and shouldn't terminate them.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{APP_EXE, env::SetupEnv, paths};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunningPhase {
    pub pid: u32,
    pub path: PathBuf,
}

/// The answer to the request to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum QuitOutcome {
    /// All processes finished.
    Closed,
    /// This Phase version can't accept the request: close it manually.
    Unsupported,
    /// Phase didn't close in the allotted time (for example, the user kept the room).
    StillRunning,
}

/// All `Phase.exe` processes visible to the installer (in the sandbox — only its own).
pub fn running(env: &SetupEnv) -> Vec<RunningPhase> {
    let mut found = list();
    if let Some(scope) = &env.process_scope {
        found.retain(|process| paths::is_inside(&process.path, scope));
    }
    found
}

/// Processes launched from exactly the install folder.
pub fn running_in(env: &SetupEnv, dir: &Path) -> Vec<RunningPhase> {
    let exe = dir.join(APP_EXE);
    running(env)
        .into_iter()
        .filter(|process| paths::same_path(&process.path, &exe))
        .collect()
}

#[cfg(windows)]
pub use win::{process_alive, request_quit, wait_for_exit};

#[cfg(windows)]
fn list() -> Vec<RunningPhase> {
    win::list()
}

#[cfg(not(windows))]
fn list() -> Vec<RunningPhase> {
    Vec::new()
}

#[cfg(windows)]
mod win {
    use std::{
        ffi::OsString,
        os::windows::ffi::OsStringExt,
        time::{Duration, Instant},
    };

    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                EVENT_MODIFY_STATE, OpenEventW, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW,
                SetEvent, WaitForSingleObject,
            },
        },
    };

    use super::*;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    pub(super) fn list() -> Vec<RunningPhase> {
        let mut found = Vec::new();
        // SAFETY: the snapshot is closed below, the structure is initialized with the right size.
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return found;
            }
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snapshot, &mut entry) != 0;
            while more {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0);
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                if name.eq_ignore_ascii_case(APP_EXE)
                    && let Some(path) = image_path(entry.th32ProcessID)
                {
                    found.push(RunningPhase {
                        pid: entry.th32ProcessID,
                        path,
                    });
                }
                more = Process32NextW(snapshot, &mut entry) != 0;
            }
            CloseHandle(snapshot);
        }
        found
    }

    fn image_path(pid: u32) -> Option<PathBuf> {
        // SAFETY: the handle is closed, the buffer size is passed in characters.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return None;
            }
            let mut buffer = vec![0u16; 4096];
            let mut size = buffer.len() as u32;
            let ok = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut size,
            );
            CloseHandle(process);
            (ok != 0).then(|| PathBuf::from(OsString::from_wide(&buffer[..size as usize])))
        }
    }

    /// Asks a process to close via its named event. `false` — there is no such event:
    /// the Phase version is old.
    pub fn request_quit(pid: u32) -> bool {
        let name = wide(&format!("Local\\PhaseQuitRequest-{pid}"));
        // SAFETY: the name is NUL-terminated, the handle is closed.
        unsafe {
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
            if event.is_null() {
                return false;
            }
            let ok = SetEvent(event) != 0;
            CloseHandle(event);
            ok
        }
    }

    pub fn process_alive(pid: u32) -> bool {
        // SAFETY: the handle is closed.
        unsafe {
            let process: HANDLE = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if process.is_null() {
                return false;
            }
            let alive = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
            CloseHandle(process);
            alive
        }
    }

    /// Waits for the process to finish for no longer than `timeout`. `true` — it finished.
    pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while process_alive(pid) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        true
    }
}

/// Phase copies that get in the way of the operation: from the `dir` folder or, if there is none, all.
pub fn blocking(env: &SetupEnv, dir: Option<&Path>) -> Vec<RunningPhase> {
    match dir {
        Some(dir) => running_in(env, dir),
        None => running(env),
    }
}

/// Asks the interfering Phase copies to close and waits up to `timeout`.
#[cfg(windows)]
pub fn close_gracefully(
    env: &SetupEnv,
    dir: Option<&Path>,
    timeout: std::time::Duration,
    cancel: &std::sync::atomic::AtomicBool,
) -> QuitOutcome {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    let processes = blocking(env, dir);
    if processes.is_empty() {
        return QuitOutcome::Closed;
    }
    let mut asked = false;
    for process in &processes {
        asked |= request_quit(process.pid);
    }
    if !asked {
        return QuitOutcome::Unsupported;
    }
    let deadline = Instant::now() + timeout;
    loop {
        if blocking(env, dir).is_empty() {
            return QuitOutcome::Closed;
        }
        if Instant::now() >= deadline || cancel.load(Ordering::Relaxed) {
            return QuitOutcome::StillRunning;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
