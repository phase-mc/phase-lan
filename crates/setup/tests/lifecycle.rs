//! The full installer cycle in a separate folder with an in-memory registry: real files,
//! real shortcuts, not a single access to the user's installation.

#![cfg(windows)]

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use phase_setup::{
    APP_EXE, MANIFEST_FILE, UNINSTALL_EXE,
    env::SetupEnv,
    error::SetupError,
    existing::{self, InstallKind, InstallMode},
    install::{self, InstallRequest, Product},
    integration::{
        CLASSES_COMMAND_KEY, CLASSES_ICON_KEY, CLASSES_KEY, LANGUAGE_VALUE, PRODUCT_KEY, RUN_KEY,
        UNINSTALL_KEY, VENDOR_KEY, autostart_command, deep_link_command,
    },
    manifest::Manifest,
    payload::{Payload, to_hex},
    procs,
    reg::{MemReg, RegStore, RegVal, WinReg},
    shortcut,
    uninstall::{self, UninstallRequest},
};
use sha2::{Digest, Sha256};

const NEW_APP: &[u8] = b"new phase program bytes";

struct Packed {
    packed: Vec<u8>,
    packed_sha: String,
    raw_sha: String,
    raw_size: u64,
}

fn pack(raw: &[u8]) -> Packed {
    let mut packed = Vec::new();
    {
        let mut writer = brotli::CompressorWriter::new(&mut packed, 4096, 5, 20);
        writer.write_all(raw).unwrap();
    }
    Packed {
        packed_sha: to_hex(&Sha256::digest(&packed)),
        raw_sha: to_hex(&Sha256::digest(raw)),
        raw_size: raw.len() as u64,
        packed,
    }
}

impl Packed {
    fn payload(&self) -> Payload<'_> {
        Payload {
            packed: &self.packed,
            packed_sha256: &self.packed_sha,
            raw_sha256: &self.raw_sha,
            raw_size: self.raw_size,
        }
    }
}

struct Lab {
    root: PathBuf,
    reg: Arc<MemReg>,
    env: SetupEnv,
    packed: Packed,
    uninstaller: PathBuf,
}

impl Lab {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("phase-setup-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let reg = Arc::new(MemReg::default());
        let env = SetupEnv::in_folder(&root, reg.clone());
        env.create_folders().unwrap();
        let uninstaller = root.join("setup-source.exe");
        fs::write(&uninstaller, b"setup program bytes").unwrap();
        Self {
            root,
            reg,
            env,
            packed: pack(NEW_APP),
            uninstaller,
        }
    }

    fn dir(&self) -> PathBuf {
        self.env.default_install_dir()
    }

    fn product(&self) -> Product<'_> {
        Product {
            version: "2.0.0",
            payload: self.packed.payload(),
            uninstaller: self.uninstaller.clone(),
        }
    }

    fn request(&self) -> InstallRequest {
        InstallRequest {
            dir: self.dir().display().to_string(),
            desktop: true,
            autostart: false,
            language: "en".to_string(),
            language_explicit: true,
        }
    }

    fn install(&self, request: &InstallRequest) -> Result<PathBuf, SetupError> {
        let cancel = AtomicBool::new(false);
        install::install(&self.env, &self.product(), request, &cancel, &mut |_| {})
    }

    fn uninstall(&self, dir: &Path, delete_data: bool) -> Result<(), SetupError> {
        uninstall::uninstall(
            &self.env,
            &UninstallRequest {
                dir: dir.to_path_buf(),
                delete_data,
                running_exe: None,
            },
        )
        .map(|_| ())
    }

    /// All the lab's files: path and contents.
    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, out);
                } else {
                    let name = path.strip_prefix(root).unwrap().display().to_string();
                    out.insert(name, fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.root, &self.root, &mut out);
        out
    }

    /// A previous NSIS installation: the program folder is also the data folder.
    fn seed_nsis_install(&self) -> PathBuf {
        let dir = self.env.data_dir();
        fs::create_dir_all(dir.join("profiles").join("default")).unwrap();
        fs::write(dir.join(APP_EXE), b"old nsis app").unwrap();
        fs::write(dir.join(UNINSTALL_EXE), b"old nsis uninstaller").unwrap();
        fs::write(
            dir.join("profiles").join("default").join("device.key"),
            b"KEY",
        )
        .unwrap();
        fs::write(dir.join("look.json"), b"{\"glow\":true}").unwrap();
        let set = |key: &str, name: &str, value: String| {
            self.reg.set(key, name, &RegVal::Str(value)).unwrap();
        };
        set(UNINSTALL_KEY, "DisplayVersion", "0.1.0".into());
        set(
            UNINSTALL_KEY,
            "InstallLocation",
            format!("\"{}\"", dir.display()),
        );
        set(
            UNINSTALL_KEY,
            "UninstallString",
            format!("\"{}\"", dir.join(UNINSTALL_EXE).display()),
        );
        set(PRODUCT_KEY, "", dir.display().to_string());
        dir
    }

    fn assert_user_data_intact(&self) {
        let dir = self.env.data_dir();
        assert_eq!(
            fs::read(dir.join("profiles/default/device.key")).unwrap(),
            b"KEY"
        );
        assert_eq!(fs::read(dir.join("look.json")).unwrap(), b"{\"glow\":true}");
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn fresh_install_is_tracked_in_the_manifest() {
    let lab = Lab::new("fresh");
    let dir = lab.install(&lab.request()).unwrap();
    assert_eq!(dir, lab.dir());
    assert_eq!(fs::read(dir.join(APP_EXE)).unwrap(), NEW_APP);
    assert_eq!(
        fs::read(dir.join(UNINSTALL_EXE)).unwrap(),
        b"setup program bytes"
    );

    let manifest = Manifest::load(&dir).unwrap().unwrap();
    assert_eq!(manifest.version, "2.0.0");
    assert_eq!(manifest.files.len(), 2);
    assert_eq!(manifest.files[0].sha256, lab.packed.raw_sha);
    assert!(manifest.shortcuts.start_menu && manifest.shortcuts.desktop);
    assert!(manifest.language_created);

    // The folder holds only our files, no service folder is left.
    let mut names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, [APP_EXE, MANIFEST_FILE, UNINSTALL_EXE]);

    let exe = dir.join(APP_EXE);
    assert!(shortcut::points_to(
        &existing::start_menu_link(&lab.env),
        &exe
    ));
    assert!(shortcut::points_to(&existing::desktop_link(&lab.env), &exe));
    assert_eq!(
        lab.reg.get_str(CLASSES_COMMAND_KEY, "").unwrap(),
        deep_link_command(&exe)
    );
    assert_eq!(
        lab.reg.get_str(UNINSTALL_KEY, "DisplayVersion").unwrap(),
        "2.0.0"
    );
    assert_eq!(
        lab.reg
            .get_str(UNINSTALL_KEY, "QuietUninstallString")
            .unwrap(),
        format!("\"{}\" --uninstall /S", dir.join(UNINSTALL_EXE).display())
    );
    assert_eq!(lab.reg.get_str(VENDOR_KEY, LANGUAGE_VALUE).unwrap(), "en");
    assert!(
        lab.reg.get_str(RUN_KEY, "Phase").is_none(),
        "автозапуск выключен"
    );
}

#[test]
fn upgrade_over_nsis_keeps_user_data_and_location() {
    let lab = Lab::new("nsis");
    let dir = lab.seed_nsis_install();

    let plan = existing::plan(&lab.env, "2.0.0");
    assert_eq!(plan.mode, InstallMode::Update);
    assert!(plan.dir_locked);
    assert_eq!(plan.dir, dir);
    assert_eq!(plan.existing.as_ref().unwrap().kind, InstallKind::Nsis);

    let mut request = lab.request();
    request.dir = plan.dir.display().to_string();
    lab.install(&request).unwrap();

    assert_eq!(fs::read(dir.join(APP_EXE)).unwrap(), NEW_APP);
    assert_eq!(
        fs::read(dir.join(UNINSTALL_EXE)).unwrap(),
        b"setup program bytes"
    );
    lab.assert_user_data_intact();
    assert_eq!(
        existing::detect(&lab.env).unwrap().kind,
        InstallKind::Managed
    );

    // A repeat launch of the same version is a reinstall, the data is intact again.
    assert_eq!(
        existing::plan(&lab.env, "2.0.0").mode,
        InstallMode::Reinstall
    );
    lab.install(&request).unwrap();
    lab.assert_user_data_intact();
}

#[test]
fn uninstall_keeps_user_data_by_default() {
    let lab = Lab::new("keep");
    let dir = lab.seed_nsis_install();
    let mut request = lab.request();
    request.dir = dir.display().to_string();
    lab.install(&request).unwrap();
    fs::write(dir.join("my-notes.txt"), b"mine").unwrap();

    lab.uninstall(&dir, false).unwrap();

    assert!(!dir.join(APP_EXE).exists());
    assert!(!dir.join(UNINSTALL_EXE).exists());
    assert!(!dir.join(MANIFEST_FILE).exists());
    assert_eq!(
        fs::read(dir.join("my-notes.txt")).unwrap(),
        b"mine",
        "чужой файл"
    );
    lab.assert_user_data_intact();
    assert!(!existing::start_menu_link(&lab.env).exists());
    assert!(!existing::desktop_link(&lab.env).exists());
    assert!(!lab.reg.key_exists(UNINSTALL_KEY));
    assert!(lab.reg.get_str(CLASSES_COMMAND_KEY, "").is_none());
    // Without consent to delete data, the language choice stays.
    assert_eq!(lab.reg.get_str(VENDOR_KEY, LANGUAGE_VALUE).unwrap(), "en");
}

/// Invite links from the site open `phase://join/<code>`: Windows hands them to Phase only through
/// the user's own `Software\Classes\phase` key (no administrator rights), and uninstalling removes
/// it without leaving empty keys behind.
#[test]
fn invite_links_are_registered_per_user_and_removed_cleanly() {
    let lab = Lab::new("protocol");
    let dir = lab.install(&lab.request()).unwrap();
    let exe = dir.join(APP_EXE);
    assert_eq!(
        lab.reg.get_str(CLASSES_KEY, "URL Protocol").as_deref(),
        Some(""),
        "без этого значения Windows не считает ключ протоколом"
    );
    assert_eq!(
        lab.reg.get_str(CLASSES_KEY, "").as_deref(),
        Some("URL:app.phase.desktop protocol")
    );
    assert_eq!(
        lab.reg.get_str(CLASSES_COMMAND_KEY, "").unwrap(),
        format!("\"{}\" \"%1\"", exe.display())
    );
    assert_eq!(
        lab.reg.get_str(CLASSES_ICON_KEY, "").unwrap(),
        format!("{},0", exe.display())
    );

    lab.uninstall(&dir, false).unwrap();
    for key in [CLASSES_COMMAND_KEY, CLASSES_ICON_KEY, CLASSES_KEY] {
        assert!(!lab.reg.key_exists(key), "{key} остался после удаления");
    }
}

/// Another copy (a portable one, say) took the links over after the install: uninstalling this
/// copy leaves that registration alone, links keep opening the other copy.
#[test]
fn uninstall_leaves_links_of_another_copy() {
    let lab = Lab::new("protocol-other");
    let dir = lab.install(&lab.request()).unwrap();
    let other = r#""D:\Games\Phase\Phase.exe" "%1""#;
    lab.reg
        .set(CLASSES_COMMAND_KEY, "", &RegVal::Str(other.into()))
        .unwrap();

    lab.uninstall(&dir, false).unwrap();
    assert_eq!(
        lab.reg.get_str(CLASSES_COMMAND_KEY, "").as_deref(),
        Some(other)
    );
    assert_eq!(
        lab.reg.get_str(CLASSES_KEY, "URL Protocol").as_deref(),
        Some("")
    );
}

#[test]
fn deleting_data_needs_the_flag_and_language_goes_only_if_ours() {
    let lab = Lab::new("purge");
    let dir = lab.seed_nsis_install();
    let webview = lab.env.local_appdata.join(phase_setup::APP_IDENTIFIER);
    fs::create_dir_all(&webview).unwrap();
    fs::write(webview.join("x"), b"1").unwrap();
    let mut request = lab.request();
    request.dir = dir.display().to_string();
    lab.install(&request).unwrap();

    lab.uninstall(&dir, true).unwrap();
    assert!(!dir.exists(), "папка данных удалена по явному согласию");
    assert!(!webview.exists());
    assert!(
        lab.reg.get_str(VENDOR_KEY, LANGUAGE_VALUE).is_none(),
        "значение создано установщиком"
    );
    assert!(!lab.reg.key_exists(VENDOR_KEY));

    // The value existed before the installer: not ours, it stays.
    let lab = Lab::new("purge-foreign");
    lab.reg
        .set(VENDOR_KEY, LANGUAGE_VALUE, &RegVal::Str("ru".into()))
        .unwrap();
    let dir = lab.install(&lab.request()).unwrap();
    assert_eq!(
        lab.reg.get_str(VENDOR_KEY, LANGUAGE_VALUE).unwrap(),
        "en",
        "явный выбор"
    );
    assert!(!Manifest::load(&dir).unwrap().unwrap().language_created);
    lab.uninstall(&dir, true).unwrap();
    assert_eq!(lab.reg.get_str(VENDOR_KEY, LANGUAGE_VALUE).unwrap(), "en");
}

#[test]
fn silent_style_update_does_not_overwrite_the_language() {
    let lab = Lab::new("lang");
    lab.reg
        .set(VENDOR_KEY, LANGUAGE_VALUE, &RegVal::Str("ru".into()))
        .unwrap();
    let mut request = lab.request();
    request.language_explicit = false;
    lab.install(&request).unwrap();
    assert_eq!(lab.reg.get_str(VENDOR_KEY, LANGUAGE_VALUE).unwrap(), "ru");
}

#[test]
fn any_failure_rolls_everything_back_on_fresh_install() {
    for step in [
        "verify",
        "folder",
        "extract",
        "uninstaller",
        "files",
        "shortcuts",
        "registry",
        "manifest",
        "finish",
    ] {
        let mut lab = Lab::new(&format!("fail-fresh-{step}"));
        let files = lab.files();
        let keys = lab.reg.key_names();
        lab.env.fault = Some(step.to_string());
        let error = lab.install(&lab.request()).unwrap_err();
        assert!(matches!(error, SetupError::Io { .. }), "{step}: {error:?}");
        assert_eq!(lab.files(), files, "{step}: файлы");
        assert_eq!(lab.reg.key_names(), keys, "{step}: реестр");
        assert!(!lab.dir().exists(), "{step}: папка установки");
    }
}

#[test]
fn any_failure_rolls_everything_back_on_update() {
    for step in ["files", "shortcuts", "registry", "manifest", "finish"] {
        let mut lab = Lab::new(&format!("fail-update-{step}"));
        let dir = lab.seed_nsis_install();
        // The old installation's shortcut and the enabled autostart must come back as they were.
        shortcut::create(&existing::desktop_link(&lab.env), &dir.join(APP_EXE), "x").unwrap();
        lab.reg
            .set(
                RUN_KEY,
                "Phase",
                &RegVal::Str(autostart_command(&dir.join(APP_EXE))),
            )
            .unwrap();
        let files = lab.files();
        let keys = lab.reg.key_names();
        let version = lab.reg.get_str(UNINSTALL_KEY, "DisplayVersion");

        lab.env.fault = Some(step.to_string());
        let mut request = lab.request();
        request.dir = dir.display().to_string();
        request.autostart = false;
        request.desktop = false;
        assert!(lab.install(&request).is_err(), "{step}");

        assert_eq!(lab.files(), files, "{step}: файлы");
        assert_eq!(lab.reg.key_names(), keys, "{step}: ключи");
        assert_eq!(lab.reg.get_str(UNINSTALL_KEY, "DisplayVersion"), version);
        assert!(
            lab.reg.get_str(RUN_KEY, "Phase").is_some(),
            "{step}: автозапуск"
        );
        lab.assert_user_data_intact();
    }
}

#[test]
fn cancelling_during_extraction_rolls_back() {
    let lab = Lab::new("cancel");
    let files = lab.files();
    let cancel = AtomicBool::new(false);
    let result = install::install(
        &lab.env,
        &lab.product(),
        &lab.request(),
        &cancel,
        &mut |p| {
            if matches!(p.stage, install::Stage::Verify) {
                cancel.store(true, Ordering::Relaxed);
            }
        },
    );
    assert_eq!(result.unwrap_err(), SetupError::Cancelled);
    assert_eq!(lab.files(), files);
    assert!(!lab.dir().exists());
}

#[test]
fn failed_uninstall_restores_everything() {
    let mut lab = Lab::new("fail-uninstall");
    let dir = lab.seed_nsis_install();
    let mut request = lab.request();
    request.dir = dir.display().to_string();
    lab.install(&request).unwrap();
    let files = lab.files();
    let keys = lab.reg.key_names();
    for step in [
        "uninstall-shortcuts",
        "uninstall-registry",
        "uninstall-files",
    ] {
        lab.env.fault = Some(step.to_string());
        assert!(lab.uninstall(&dir, true).is_err(), "{step}");
        assert_eq!(lab.files(), files, "{step}");
        assert_eq!(lab.reg.key_names(), keys, "{step}");
    }
}

#[test]
fn corrupted_payload_never_reaches_the_disk() {
    let mut lab = Lab::new("corrupt");
    lab.packed.packed[2] ^= 0xff;
    assert_eq!(
        lab.install(&lab.request()).unwrap_err(),
        SetupError::CorruptPayload
    );
    assert!(!lab.dir().exists());

    // The compressed data matched but the unpacked file is different: also a refusal, also without traces.
    let mut lab = Lab::new("corrupt-raw");
    lab.packed.raw_sha = "00".repeat(32);
    assert_eq!(
        lab.install(&lab.request()).unwrap_err(),
        SetupError::CorruptPayload
    );
    assert!(!lab.dir().exists());
    assert!(lab.reg.key_names().is_empty());
}

#[test]
fn rejects_bad_targets_before_writing() {
    let lab = Lab::new("paths");
    let mut request = lab.request();
    request.dir = r"relative\Phase".to_string();
    assert!(matches!(
        lab.install(&request).unwrap_err(),
        SetupError::InvalidPath { .. }
    ));

    // Not allowed inside the data folder: deleting the data would take the installation down.
    request.dir = lab.env.data_dir().join("sub").display().to_string();
    assert!(matches!(
        lab.install(&request).unwrap_err(),
        SetupError::InvalidPath { .. }
    ));

    // Installed elsewhere: we don't create a second copy.
    lab.install(&lab.request()).unwrap();
    request.dir = lab.root.join("Elsewhere").display().to_string();
    assert!(matches!(
        lab.install(&request).unwrap_err(),
        SetupError::InstalledElsewhere { .. }
    ));
    assert!(!lab.root.join("Elsewhere").exists());
}

#[test]
fn disk_and_permission_problems_are_reported() {
    let mut lab = Lab::new("space");
    lab.env.fault = Some("no-space".to_string());
    assert!(matches!(
        lab.install(&lab.request()).unwrap_err(),
        SetupError::NotEnoughDisk { .. }
    ));
    let check = install::check_dir(&lab.env, &lab.product(), &lab.dir().display().to_string());
    assert!(!check.ok && !check.enough_space);

    lab.env.fault = Some("no-write".to_string());
    assert!(matches!(
        lab.install(&lab.request()).unwrap_err(),
        SetupError::NoWriteAccess { .. }
    ));
    let check = install::check_dir(&lab.env, &lab.product(), &lab.dir().display().to_string());
    assert!(!check.ok && !check.writable);
    assert!(!lab.dir().exists());
}

#[test]
fn autostart_follows_the_choice_and_leaves_other_copies_alone() {
    let lab = Lab::new("autostart");
    let mut request = lab.request();
    request.autostart = true;
    let dir = lab.install(&request).unwrap();
    let exe = dir.join(APP_EXE);
    assert_eq!(
        lab.reg.get_str(RUN_KEY, "Phase").unwrap(),
        autostart_command(&exe)
    );
    assert!(existing::plan(&lab.env, "2.0.0").autostart);

    request.autostart = false;
    lab.install(&request).unwrap();
    assert!(lab.reg.get_str(RUN_KEY, "Phase").is_none());

    // Another Phase copy's autostart isn't ours: it stays both on install and on uninstall.
    let other = r"C:\Phase\Phase.exe --autostart";
    lab.reg
        .set(RUN_KEY, "Phase", &RegVal::Str(other.into()))
        .unwrap();
    lab.install(&request).unwrap();
    assert_eq!(lab.reg.get_str(RUN_KEY, "Phase").unwrap(), other);
    lab.uninstall(&dir, false).unwrap();
    assert_eq!(lab.reg.get_str(RUN_KEY, "Phase").unwrap(), other);
}

#[test]
fn foreign_shortcuts_are_left_alone() {
    let lab = Lab::new("links");
    let other = lab.root.join("Other").join(APP_EXE);
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(&other, b"portable copy").unwrap();
    let link = existing::desktop_link(&lab.env);
    shortcut::create(&link, &other, "x").unwrap();
    let before = fs::read(&link).unwrap();

    let dir = lab.install(&lab.request()).unwrap();
    assert_eq!(
        fs::read(&link).unwrap(),
        before,
        "чужой ярлык не перезаписан"
    );
    assert!(!Manifest::load(&dir).unwrap().unwrap().shortcuts.desktop);

    lab.uninstall(&dir, false).unwrap();
    assert_eq!(fs::read(&link).unwrap(), before, "чужой ярлык не удалён");
}

#[test]
fn running_phase_blocks_and_is_never_killed() {
    let lab = Lab::new("running");
    let dir = lab.install(&lab.request()).unwrap();
    // A harmless system process under the name Phase.exe inside the lab.
    let system = std::env::var("SystemRoot").unwrap();
    let stand_in = dir.join(APP_EXE);
    fs::copy(
        Path::new(&system).join("System32").join("ping.exe"),
        &stand_in,
    )
    .unwrap();
    let mut child = std::process::Command::new(&stand_in)
        .args(["-n", "60", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let error = lab.install(&lab.request()).unwrap_err();
    assert!(
        matches!(error, SetupError::PhaseRunning { .. }),
        "{error:?}"
    );
    assert!(matches!(
        lab.uninstall(&dir, false).unwrap_err(),
        SetupError::PhaseRunning { .. }
    ));
    // The process can't accept the request: the installer acknowledges this and doesn't touch it.
    let cancel = AtomicBool::new(false);
    let outcome = procs::close_gracefully(
        &lab.env,
        Some(&dir),
        std::time::Duration::from_millis(200),
        &cancel,
    );
    assert_eq!(outcome, procs::QuitOutcome::Unsupported);
    assert!(child.try_wait().unwrap().is_none(), "процесс жив");

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn real_registry_backend_stays_inside_its_sandbox_key() {
    let name = format!(r"Software\PhaseSetupSandbox\test-{}", std::process::id());
    let reg = WinReg::sandboxed(&name);
    let key = r"Software\Phase\Deep\Key";
    reg.set(key, "text", &RegVal::Str("значение".into()))
        .unwrap();
    reg.set(key, "number", &RegVal::Dword(7)).unwrap();
    reg.set(key, "bytes", &RegVal::Bin(vec![1, 2, 3])).unwrap();
    reg.set(key, "", &RegVal::Str("default".into())).unwrap();
    assert_eq!(reg.get_str(key, "text").unwrap(), "значение");
    assert_eq!(reg.get(key, "number").unwrap(), Some(RegVal::Dword(7)));
    assert_eq!(
        reg.get(key, "bytes").unwrap(),
        Some(RegVal::Bin(vec![1, 2, 3]))
    );
    assert_eq!(reg.get_str(key, "").unwrap(), "default");
    assert_eq!(reg.value_names(key).unwrap().len(), 4);
    assert!(!reg.delete_key_if_empty(key).unwrap(), "ключ не пуст");
    for value in ["text", "number", "bytes", ""] {
        reg.delete_value(key, value).unwrap();
    }
    assert!(reg.delete_key_if_empty(key).unwrap());
    for parent in [r"Software\Phase\Deep", r"Software\Phase", "Software"] {
        assert!(reg.delete_key_if_empty(parent).unwrap(), "{parent}");
    }
    assert!(!reg.key_exists("Software"));
    let root = WinReg::real();
    assert!(root.delete_key_if_empty(&name).unwrap());
    let _ = root.delete_key_if_empty(r"Software\PhaseSetupSandbox");
}
