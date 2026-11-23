//! Uninstall. Only what is in the manifest is removed (or the two NSIS install files),
//! shortcuts and registry entries pointing to this folder. User data — only with explicit
//! consent, and that is the last, already irreversible step.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use crate::{
    APP_EXE, APP_IDENTIFIER, UNINSTALL_EXE,
    env::SetupEnv,
    error::{Result, SetupError},
    existing, integration,
    manifest::Manifest,
    paths, procs, shortcut,
    tx::{Tx, WORK_PREFIX},
};

/// The start of the name of the installer copy that finishes the uninstall.
const CLEANUP_PREFIX: &str = "Phase-Cleanup-";
/// The start of the name of the installer window's temporary WebView2 folder.
pub const WEBVIEW_PREFIX: &str = "Phase-Setup-WebView-";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone)]
pub struct UninstallRequest {
    pub dir: PathBuf,
    pub delete_data: bool,
    /// The running file: it can't be deleted from itself.
    pub running_exe: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UninstallOutcome {
    /// The uninstaller stayed on disk until the process ends.
    pub self_pending: bool,
    /// What of the data couldn't be deleted (the program itself was removed).
    pub data_errors: Vec<String>,
}

/// Deletes the folder's contents except `keep`, and the folder itself if it became empty.
fn remove_tree_except(dir: &Path, keep: Option<&Path>, errors: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return,
        Err(err) => {
            errors.push(format!("{}: {err}", dir.display()));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.is_some_and(|keep| paths::same_path(keep, &path)) {
            continue;
        }
        // file_type doesn't follow links: the link is deleted, its target stays.
        let result = match entry.file_type() {
            Ok(kind) if kind.is_dir() => fs::remove_dir_all(&path),
            _ => fs::remove_file(&path),
        };
        if let Err(err) = result {
            errors.push(format!("{}: {err}", path.display()));
        }
    }
    let _ = fs::remove_dir(dir);
}

pub fn uninstall(env: &SetupEnv, request: &UninstallRequest) -> Result<UninstallOutcome> {
    let dir = &request.dir;
    let manifest = match Manifest::load(dir) {
        Ok(Some(manifest)) => manifest,
        Ok(None) if dir.join(APP_EXE).is_file() || dir.join(UNINSTALL_EXE).is_file() => {
            Manifest::for_nsis_install()
        }
        Ok(None) => return Err(SetupError::NotInstalled),
        Err(err) => {
            return Err(SetupError::Io {
                step: "manifest".into(),
                detail: err.to_string(),
            });
        }
    };

    let in_folder = procs::running_in(env, dir);
    if !in_folder.is_empty() {
        return Err(SetupError::PhaseRunning {
            pids: in_folder.iter().map(|process| process.pid).collect(),
        });
    }
    // The data is used by any Phase copy, including one from another folder.
    if request.delete_data {
        let anywhere = procs::running(env);
        if !anywhere.is_empty() {
            return Err(SetupError::PhaseRunning {
                pids: anywhere.iter().map(|process| process.pid).collect(),
            });
        }
    }

    let work_dir = dir.join(format!("{WORK_PREFIX}{}", std::process::id()));
    let io_error = |step: &str, path: &Path, err: io::Error| SetupError::from_io(step, path, &err);
    fs::create_dir(&work_dir).map_err(|err| io_error("folder", &work_dir, err))?;
    let mut tx = Tx::new(env, work_dir.clone());

    // Shortcuts: only those pointing to this installation.
    let exe = dir.join(APP_EXE);
    for link in [existing::start_menu_link(env), existing::desktop_link(env)] {
        if shortcut::points_to(&link, &exe) {
            tx.set_aside(&link)
                .map_err(|err| io_error("shortcuts", &link, err))?;
        }
    }
    env.checkpoint("uninstall-shortcuts")
        .map_err(|err| io_error("shortcuts", dir, err))?;

    integration::unregister(
        env,
        &mut tx,
        dir,
        request.delete_data,
        manifest.language_created,
    )
    .map_err(|err| io_error("registry", Path::new("HKCU"), err))?;
    env.checkpoint("uninstall-registry")
        .map_err(|err| io_error("registry", dir, err))?;

    // Files by the manifest. The running uninstaller will be deleted by a helper process.
    let mut self_pending = false;
    for file in &manifest.files {
        let Some(relative) = paths::safe_relative(&file.path) else {
            continue;
        };
        let target = dir.join(relative);
        if request
            .running_exe
            .as_deref()
            .is_some_and(|running| paths::same_path(running, &target))
        {
            self_pending = true;
            continue;
        }
        tx.set_aside(&target)
            .map_err(|err| io_error("files", &target, err))?;
    }
    let manifest_path = Manifest::path_in(dir);
    tx.set_aside(&manifest_path)
        .map_err(|err| io_error("files", &manifest_path, err))?;
    env.checkpoint("uninstall-files")
        .map_err(|err| io_error("files", dir, err))?;

    // The point of no return.
    tx.commit();
    integration::remove_empty_keys(env.reg.as_ref());
    let _ = fs::remove_dir(dir);

    let mut data_errors = Vec::new();
    if request.delete_data {
        let keep = self_pending.then(|| dir.join(UNINSTALL_EXE));
        remove_tree_except(&env.data_dir(), keep.as_deref(), &mut data_errors);
        remove_tree_except(
            &env.local_appdata.join(APP_IDENTIFIER),
            None,
            &mut data_errors,
        );
        remove_tree_except(
            &env.roaming_appdata.join(APP_IDENTIFIER),
            None,
            &mut data_errors,
        );
    }
    Ok(UninstallOutcome {
        self_pending,
        data_errors,
    })
}

/// Starts the final step: it waits for this process to exit and removes the uninstaller
/// and the window's temporary folder. A running file can't delete itself, so the step runs from
/// a copy in the temporary folder. The copy itself stays there until the installer's next launch
/// or a Windows temp cleanup: deleting itself via `cmd` would look like
/// malware behavior, and it matters more for an installer not to raise antivirus
/// suspicion.
pub fn spawn_finish(
    env: &SetupEnv,
    running_exe: &Path,
    uninstall_dir: Option<&Path>,
    webview: Option<&Path>,
) -> io::Result<()> {
    let program = if uninstall_dir.is_some() {
        fs::create_dir_all(&env.temp)?;
        let copy = env
            .temp
            .join(format!("{CLEANUP_PREFIX}{}.exe", std::process::id()));
        fs::copy(running_exe, &copy)?;
        copy
    } else {
        running_exe.to_path_buf()
    };
    let mut command = Command::new(program);
    command
        .arg("--finish")
        .arg("--wait-pid")
        .arg(std::process::id().to_string());
    if let Some(dir) = uninstall_dir {
        command.arg("--dir").arg(dir);
    }
    if let Some(webview) = webview {
        command.arg("--webview").arg(webview);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = CREATE_NO_WINDOW;
    command.spawn().map(|_| ())
}

fn retry_for(limit: Duration, mut action: impl FnMut() -> bool) {
    let deadline = Instant::now() + limit;
    while !action() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The final step after process `wait_pid` exits.
pub fn finish(env: &SetupEnv, wait_pid: u32, dir: Option<&Path>, webview: Option<&Path>) {
    #[cfg(windows)]
    procs::wait_for_exit(wait_pid, Duration::from_secs(24 * 60 * 60));
    #[cfg(not(windows))]
    let _ = wait_pid;

    if let Some(dir) = dir {
        // A manifest appeared — Phase was installed again in the meantime: that's someone else's file now.
        if !Manifest::path_in(dir).exists() {
            let target = dir.join(UNINSTALL_EXE);
            retry_for(Duration::from_secs(10), || {
                !target.exists() || fs::remove_file(&target).is_ok()
            });
            let _ = fs::remove_dir(dir);
        }
    }
    if let Some(webview) = webview {
        finish_webview(env, webview);
    }
    sweep_cleanup_copies(env);
}

fn finish_webview(env: &SetupEnv, webview: &Path) {
    // Only our temporary folder and only directly in the temp directory.
    let ours = webview
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with(WEBVIEW_PREFIX))
        && webview
            .parent()
            .is_some_and(|parent| paths::same_path(parent, &env.temp));
    if ours {
        // WebView2 processes don't end right after the window.
        retry_for(Duration::from_secs(15), || {
            !webview.exists() || fs::remove_dir_all(webview).is_ok()
        });
    }
}

/// Removes copies left from past uninstalls. A busy (current) one just won't be deleted.
pub fn sweep_cleanup_copies(env: &SetupEnv) {
    let Ok(entries) = fs::read_dir(&env.temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(CLEANUP_PREFIX) && name.ends_with(".exe") {
            let _ = fs::remove_file(entry.path());
        }
    }
}
