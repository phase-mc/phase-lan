//! Install and update. The order is such that a failure at any step leaves everything as it was:
//! first everything is prepared in a temporary folder next to the target, then the [`Tx`] journal puts
//! files, shortcuts and registry entries in place, and only at the end is it committed.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    APP_EXE, APP_IDENTIFIER, MANIFEST_FILE, UNINSTALL_EXE,
    env::SetupEnv,
    error::{Result, SetupError},
    existing,
    integration::{self, LANGUAGE_VALUE, Registration, VENDOR_KEY},
    manifest::{Manifest, OwnedFile, OwnedShortcuts},
    paths::{self, PathProblem},
    payload::{Payload, to_hex},
    procs,
    reg::RegVal,
    shortcut,
    tx::{Tx, WORK_PREFIX},
};

/// Extra space beyond the file sizes: temporary copies and file system records.
const SPACE_MARGIN: u64 = 16 * 1024 * 1024;
const COPY_CHUNK: usize = 256 * 1024;

pub struct Product<'a> {
    pub version: &'a str,
    pub payload: Payload<'a>,
    /// The file that will become `uninstall.exe` (the installer itself).
    pub uninstaller: PathBuf,
}

#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub dir: String,
    pub desktop: bool,
    pub autostart: bool,
    pub language: String,
    /// The language was chosen in the window. In silent mode an existing choice isn't overwritten.
    pub language_explicit: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Verify,
    Extract,
    Uninstaller,
    Files,
    Shortcuts,
    Registry,
    Finish,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Progress {
    pub stage: Stage,
    /// Fraction of the whole install, from 0 to 1.
    pub overall: f32,
}

/// Step shares in the overall bar: unpacking is the longest.
const VERIFY: (f32, f32) = (0.0, 0.08);
const EXTRACT: (f32, f32) = (0.08, 0.78);
const UNINSTALLER: (f32, f32) = (0.78, 0.92);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirCheck {
    pub ok: bool,
    pub dir: String,
    pub problem: Option<PathProblem>,
    pub writable: bool,
    pub free_bytes: Option<u64>,
    pub need_bytes: u64,
    pub enough_space: bool,
    /// The folder already has a `Phase.exe` that doesn't belong to a known installation.
    pub replaces_foreign_exe: bool,
}

/// The nearest existing folder on the path to `dir`.
fn existing_ancestor(dir: &Path) -> Option<&Path> {
    dir.ancestors().find(|path| path.is_dir())
}

#[cfg(windows)]
pub fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = existing_ancestor(dir)?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut free = 0u64;
    // SAFETY: a NUL-terminated string; the optional pointers are null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
pub fn free_space(_dir: &Path) -> Option<u64> {
    None
}

fn free_space_for(env: &SetupEnv, dir: &Path) -> Option<u64> {
    if cfg!(debug_assertions) && env.fault.as_deref() == Some("no-space") {
        return Some(1024 * 1024);
    }
    free_space(dir)
}

/// Whether writing is possible: a probe file in the nearest existing folder.
fn writable(env: &SetupEnv, dir: &Path) -> bool {
    if cfg!(debug_assertions) && env.fault.as_deref() == Some("no-write") {
        return false;
    }
    let Some(base) = existing_ancestor(dir) else {
        return false;
    };
    let probe = base.join(format!(".phase-setup-probe-{}", std::process::id()));
    let created = File::create(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    created
}

fn needed_space(product: &Product<'_>) -> u64 {
    let uninstaller = fs::metadata(&product.uninstaller).map_or(0, |meta| meta.len());
    product.payload.raw_size + uninstaller + SPACE_MARGIN
}

/// Folder check for the window: creates nothing.
pub fn check_dir(env: &SetupEnv, product: &Product<'_>, raw: &str) -> DirCheck {
    let need_bytes = needed_space(product);
    match paths::validate_install_dir(raw, env) {
        Err(problem) => DirCheck {
            ok: false,
            dir: raw.to_string(),
            problem: Some(problem),
            writable: false,
            free_bytes: None,
            need_bytes,
            enough_space: false,
            replaces_foreign_exe: false,
        },
        Ok(dir) => {
            let free_bytes = free_space_for(env, &dir);
            let enough_space = free_bytes.is_none_or(|free| free >= need_bytes);
            let writable = writable(env, &dir);
            let known = existing::detect(env).is_some_and(|e| paths::same_path(&e.dir, &dir));
            DirCheck {
                ok: writable && enough_space,
                dir: dir.display().to_string(),
                problem: None,
                writable,
                free_bytes,
                need_bytes,
                enough_space,
                replaces_foreign_exe: !known && dir.join(APP_EXE).is_file(),
            }
        }
    }
}

struct Run<'a> {
    env: &'a SetupEnv,
    cancel: &'a AtomicBool,
    report: &'a mut dyn FnMut(Progress),
}

impl Run<'_> {
    fn step(&self, name: &str) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(SetupError::Cancelled);
        }
        self.env
            .checkpoint(name)
            .map_err(|err| SetupError::from_io(name, Path::new(name), &err))
    }

    fn progress(&mut self, stage: Stage, span: (f32, f32), part: f32) {
        (self.report)(Progress {
            stage,
            overall: span.0 + (span.1 - span.0) * part.clamp(0.0, 1.0),
        });
    }
}

/// Copies a file with a checksum and progress shares.
fn copy_with_hash(run: &mut Run<'_>, from: &Path, to: &Path) -> Result<(u64, String)> {
    let io_error = |err: io::Error| SetupError::from_io("uninstaller", to, &err);
    let mut source =
        File::open(from).map_err(|err| SetupError::from_io("uninstaller", from, &err))?;
    let total = source.metadata().map_err(io_error)?.len().max(1);
    let mut target = File::create(to).map_err(io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; COPY_CHUNK];
    let mut done = 0u64;
    loop {
        if run.cancel.load(Ordering::Relaxed) {
            return Err(SetupError::Cancelled);
        }
        let count = source
            .read(&mut buffer)
            .map_err(|err| SetupError::from_io("uninstaller", from, &err))?;
        if count == 0 {
            break;
        }
        target.write_all(&buffer[..count]).map_err(io_error)?;
        hasher.update(&buffer[..count]);
        done += count as u64;
        run.progress(Stage::Uninstaller, UNINSTALLER, done as f32 / total as f32);
    }
    target.sync_all().map_err(io_error)?;
    Ok((done, to_hex(&hasher.finalize())))
}

/// Creates or updates a shortcut. Someone else's shortcut with the same name is left alone and `false` is reported.
fn place_shortcut(tx: &mut Tx<'_>, link: &Path, exe: &Path) -> io::Result<bool> {
    if link.exists() {
        if !shortcut::points_to(link, exe) {
            return Ok(false);
        }
        tx.set_aside(link)?;
    }
    if let Some(parent) = link.parent() {
        tx.create_dir_all(parent)?;
    }
    shortcut::create(link, exe, APP_IDENTIFIER)?;
    tx.created_file(link);
    Ok(true)
}

/// Installs or updates Phase. On any error everything returns to how it was.
pub fn install(
    env: &SetupEnv,
    product: &Product<'_>,
    request: &InstallRequest,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(Progress),
) -> Result<PathBuf> {
    let dir = paths::validate_install_dir(&request.dir, env).map_err(|problem| {
        SetupError::InvalidPath {
            problem: problem.key().to_string(),
        }
    })?;
    if let Some(found) = existing::detect(env)
        && !paths::same_path(&found.dir, &dir)
    {
        return Err(SetupError::InstalledElsewhere {
            path: found.dir.display().to_string(),
        });
    }
    let running = procs::running_in(env, &dir);
    if !running.is_empty() {
        return Err(SetupError::PhaseRunning {
            pids: running.iter().map(|process| process.pid).collect(),
        });
    }

    let mut run = Run {
        env,
        cancel,
        report,
    };

    // Integrity check first: a corrupted file must not reach the disk.
    run.step("verify")?;
    {
        let report = &mut *run.report;
        product.payload.verify(cancel, &mut |part| {
            report(Progress {
                stage: Stage::Verify,
                overall: VERIFY.0 + (VERIFY.1 - VERIFY.0) * part,
            });
        })?;
    }

    let need = needed_space(product);
    if let Some(free) = free_space_for(env, &dir)
        && free < need
    {
        return Err(SetupError::NotEnoughDisk { need, free });
    }

    // Leftovers of interrupted runs (the service folders are only ours).
    sweep_work_dirs(&dir);

    let work_dir = dir.join(format!("{WORK_PREFIX}{}", std::process::id()));
    let mut tx = Tx::new(env, work_dir.clone());
    let denied = |path: &Path, err: io::Error| SetupError::from_io("folder", path, &err);
    run.step("folder")?;
    if cfg!(debug_assertions) && env.fault.as_deref() == Some("no-write") {
        return Err(SetupError::NoWriteAccess {
            path: dir.display().to_string(),
        });
    }
    tx.create_dir_all(&dir).map_err(|err| denied(&dir, err))?;
    tx.create_dir_all(&work_dir)
        .map_err(|err| denied(&work_dir, err))?;

    // Unpacking into a temporary folder on the same drive.
    run.step("extract")?;
    let staged_exe = work_dir.join(APP_EXE);
    let exe_hash = {
        let io_error = |err: io::Error| SetupError::from_io("extract", &staged_exe, &err);
        let mut file = File::create(&staged_exe).map_err(io_error)?;
        let report = &mut *run.report;
        let hash = product.payload.extract(&mut file, cancel, &mut |part| {
            report(Progress {
                stage: Stage::Extract,
                overall: EXTRACT.0 + (EXTRACT.1 - EXTRACT.0) * part,
            });
        })?;
        file.sync_all().map_err(io_error)?;
        hash
    };

    run.step("uninstaller")?;
    let staged_uninstaller = work_dir.join(UNINSTALL_EXE);
    let (uninstaller_size, uninstaller_hash) =
        copy_with_hash(&mut run, &product.uninstaller, &staged_uninstaller)?;

    // From here on — real changes. Each is written to the journal.
    run.step("files")?;
    run.progress(Stage::Files, (0.92, 0.95), 0.0);
    let old_manifest = Manifest::load(&dir).ok().flatten();
    let files = vec![
        OwnedFile {
            path: APP_EXE.to_string(),
            size: product.payload.raw_size,
            sha256: exe_hash,
        },
        OwnedFile {
            path: UNINSTALL_EXE.to_string(),
            size: uninstaller_size,
            sha256: uninstaller_hash,
        },
    ];
    for (staged, name) in [(&staged_exe, APP_EXE), (&staged_uninstaller, UNINSTALL_EXE)] {
        let target = dir.join(name);
        tx.place(staged, &target)
            .map_err(|err| SetupError::from_io("files", &target, &err))?;
    }
    // Files of the previous version that are no longer in the list (only from its manifest).
    if let Some(old) = &old_manifest {
        for file in &old.files {
            if !files
                .iter()
                .any(|new| new.path.eq_ignore_ascii_case(&file.path))
                && let Some(relative) = paths::safe_relative(&file.path)
            {
                let target = dir.join(relative);
                tx.set_aside(&target)
                    .map_err(|err| SetupError::from_io("files", &target, &err))?;
            }
        }
    }

    run.step("shortcuts")?;
    run.progress(Stage::Shortcuts, (0.95, 0.97), 0.0);
    let exe = dir.join(APP_EXE);
    let shortcut_error = |path: &Path, err: io::Error| SetupError::from_io("shortcuts", path, &err);
    let start_link = existing::start_menu_link(env);
    let start_menu = place_shortcut(&mut tx, &start_link, &exe)
        .map_err(|err| shortcut_error(&start_link, err))?;
    let desktop_link = existing::desktop_link(env);
    let desktop = if request.desktop {
        place_shortcut(&mut tx, &desktop_link, &exe)
            .map_err(|err| shortcut_error(&desktop_link, err))?
    } else {
        // The checkbox was turned off: remove only our own shortcut.
        if shortcut::points_to(&desktop_link, &exe) {
            tx.set_aside(&desktop_link)
                .map_err(|err| shortcut_error(&desktop_link, err))?;
        }
        false
    };

    run.step("registry")?;
    run.progress(Stage::Registry, (0.97, 0.99), 0.0);
    let registry_error = |err: io::Error| SetupError::from_io("registry", Path::new("HKCU"), &err);
    integration::register(
        env,
        &mut tx,
        &Registration {
            dir: &dir,
            version: product.version,
            size_bytes: product.payload.raw_size + uninstaller_size,
            autostart: request.autostart,
        },
    )
    .map_err(registry_error)?;

    // Language for the Phase window: `HKCU\Software\Phase`, value `Language`.
    let previous_language = env.reg.get_str(VENDOR_KEY, LANGUAGE_VALUE);
    let language_created = match &previous_language {
        None => true,
        Some(_) => old_manifest
            .as_ref()
            .is_some_and(|old| old.language_created),
    };
    if request.language_explicit || previous_language.is_none() {
        tx.set_value(
            VENDOR_KEY,
            LANGUAGE_VALUE,
            RegVal::Str(request.language.clone()),
        )
        .map_err(registry_error)?;
    }

    run.step("manifest")?;
    let mut manifest = Manifest::new(
        product.version,
        files,
        OwnedShortcuts {
            start_menu,
            desktop,
        },
    );
    manifest.language_created = language_created;
    let staged_manifest = work_dir.join(MANIFEST_FILE);
    let manifest_error = |err: io::Error| SetupError::from_io("manifest", &staged_manifest, &err);
    manifest
        .write_to(&staged_manifest)
        .map_err(manifest_error)?;
    tx.place(&staged_manifest, &Manifest::path_in(&dir))
        .map_err(manifest_error)?;

    // The last point where you can still change your mind.
    run.step("finish")?;
    tx.commit();
    run.progress(Stage::Finish, (0.99, 1.0), 1.0);
    Ok(dir)
}

/// Removes the service folders of interrupted runs.
fn sweep_work_dirs(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(WORK_PREFIX)
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}
