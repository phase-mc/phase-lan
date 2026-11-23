//! The Phase installer: launch parsing, silent mode and the Tauri window.
//!
//! All the logic is in the `phase_setup` library, here only the glue with the window.

// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use phase_setup::{
    APP_EXE,
    cli::{self, Command},
    env::{self, SetupEnv},
    error::SetupError,
    existing::{self, Plan},
    i18n,
    install::{self, DirCheck, InstallRequest, Product, Progress},
    payload::Payload,
    procs::{self, QuitOutcome},
    uninstall::{self, UninstallRequest, WEBVIEW_PREFIX},
    webview2,
};
use serde::{Deserialize, Serialize};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent, ipc::Channel};

include!(concat!(env!("OUT_DIR"), "/payload_meta.rs"));

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How long to wait for Phase to close on request: a person needs time to answer.
const QUIT_WAIT: Duration = Duration::from_secs(45);
/// Exit code: WebView2 is missing, installing it failed or the user declined.
const EXIT_NO_WEBVIEW: i32 = 11;
const EXIT_BAD_ARGS: i32 = 2;

fn payload() -> Payload<'static> {
    Payload {
        packed: if PAYLOAD_PRESENT { PACKED } else { &[] },
        packed_sha256: PACKED_SHA256,
        raw_sha256: RAW_SHA256,
        raw_size: RAW_SIZE,
    }
}

fn own_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("phase-setup.exe"))
}

fn product() -> Product<'static> {
    Product {
        version: VERSION,
        payload: payload(),
        uninstaller: own_exe(),
    }
}

// ---------------------------------------------------------------- system odds and ends

#[cfg(windows)]
mod native {
    use windows_sys::Win32::{
        Foundation::{ERROR_ALREADY_EXISTS, GetLastError},
        System::{
            Console::{ATTACH_PARENT_PROCESS, AttachConsole},
            Threading::CreateMutexW,
        },
        UI::WindowsAndMessaging::{
            IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO, MessageBoxW,
        },
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    fn show(title: &str, text: &str, flags: u32) -> i32 {
        let (title, text) = (wide(title), wide(text));
        // SAFETY: the strings are NUL-terminated and live for the duration of the call.
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), flags) }
    }

    pub fn error(title: &str, text: &str) {
        show(title, text, MB_OK | MB_ICONERROR);
    }

    pub fn ask(title: &str, text: &str) -> bool {
        show(title, text, MB_YESNO | MB_ICONQUESTION) == IDYES
    }

    /// Silent mode output goes to the console the installer was launched from.
    pub fn attach_console() {
        // SAFETY: a call without pointers; an error means there is no console.
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    /// Holds a named mutex until the process ends. `false` — another one is already running.
    pub fn single_instance(name: &str) -> bool {
        let name = wide(name);
        // SAFETY: the name is NUL-terminated; the handle intentionally lives until the process ends.
        unsafe {
            let mutex = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            !mutex.is_null() && GetLastError() != ERROR_ALREADY_EXISTS
        }
    }
}

fn instance_name(env: &SetupEnv) -> String {
    match &env.process_scope {
        Some(scope) => format!(
            "Local\\PhaseSetupRunning-{}",
            scope
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
        None => "Local\\PhaseSetupRunning".to_string(),
    }
}

// ---------------------------------------------------------------- silent mode

fn say(lang: &str, key: &str, fill: &[(&str, &str)]) {
    let mut text = i18n::text(lang, key);
    for (name, value) in fill {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    println!("{text}");
}

fn fail_silent(lang: &str, error: &SetupError) -> i32 {
    eprintln!("{}", error.message(lang));
    error.exit_code()
}

fn silent_install(env: &SetupEnv, dir: Option<PathBuf>) -> i32 {
    let lang = i18n::system_language();
    if !native::single_instance(&instance_name(env)) {
        return fail_silent(lang, &SetupError::AlreadyRunning);
    }
    uninstall::sweep_cleanup_copies(env);
    let plan = existing::plan(env, VERSION);
    let request = InstallRequest {
        dir: dir
            .unwrap_or_else(|| plan.dir.clone())
            .display()
            .to_string(),
        desktop: plan.desktop,
        autostart: plan.autostart,
        language: existing::initial_language(env).to_string(),
        language_explicit: false,
    };
    let cancel = AtomicBool::new(false);
    match install::install(env, &product(), &request, &cancel, &mut |_| {}) {
        Ok(path) => {
            let path = path.display().to_string();
            say(
                lang,
                "silent.installed",
                &[("version", VERSION), ("path", &path)],
            );
            0
        }
        Err(error) => fail_silent(lang, &error),
    }
}

fn uninstall_target(env: &SetupEnv) -> Option<PathBuf> {
    existing::install_dir_of_uninstaller(env, &own_exe())
        .or_else(|| existing::detect(env).map(|found| found.dir))
}

fn silent_uninstall(env: &SetupEnv, delete_data: bool) -> i32 {
    let lang = i18n::system_language();
    if !native::single_instance(&instance_name(env)) {
        return fail_silent(lang, &SetupError::AlreadyRunning);
    }
    let Some(dir) = uninstall_target(env) else {
        return fail_silent(lang, &SetupError::NotInstalled);
    };
    let exe = own_exe();
    let request = UninstallRequest {
        dir: dir.clone(),
        delete_data,
        running_exe: Some(exe.clone()),
    };
    match uninstall::uninstall(env, &request) {
        Ok(outcome) => {
            if outcome.self_pending {
                let _ = uninstall::spawn_finish(env, &exe, Some(&dir), None);
            }
            say(lang, "silent.uninstalled", &[]);
            0
        }
        Err(error) => fail_silent(lang, &error),
    }
}

/// Check of the embedded program: reads only itself, writes nothing to disk.
fn verify_payload() -> i32 {
    let lang = i18n::system_language();
    let cancel = AtomicBool::new(false);
    let payload = payload();
    let result = payload.verify(&cancel, &mut |_| {}).and_then(|()| {
        payload
            .extract(&mut std::io::sink(), &cancel, &mut |_| {})
            .map(|_| ())
    });
    match result {
        Ok(()) => {
            say(lang, "silent.payloadOk", &[]);
            0
        }
        Err(error) => fail_silent(lang, &error),
    }
}

// ---------------------------------------------------------------- window

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum WindowMode {
    Install,
    Uninstall,
}

struct Shared {
    env: Arc<SetupEnv>,
    mode: WindowMode,
    language: Mutex<String>,
    cancel: AtomicBool,
    busy: AtomicBool,
    webview_dir: PathBuf,
    /// The final step has already started (right after uninstalling).
    finish_started: AtomicBool,
    installed_exe: Mutex<Option<PathBuf>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LanguageInfo {
    code: &'static str,
    name: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Boot {
    mode: WindowMode,
    debug: bool,
    version: &'static str,
    size_bytes: u64,
    language: String,
    languages: Vec<LanguageInfo>,
    dictionaries: BTreeMap<&'static str, BTreeMap<String, String>>,
    plan: Plan,
    check: DirCheck,
    payload_present: bool,
    uninstall_dir: Option<String>,
    data_dir: String,
}

#[tauri::command]
fn boot(state: tauri::State<'_, Shared>) -> Boot {
    let env = &state.env;
    let plan = existing::plan(env, VERSION);
    let check = install::check_dir(env, &product(), &plan.dir.display().to_string());
    Boot {
        mode: state.mode,
        debug: cfg!(debug_assertions),
        version: VERSION,
        size_bytes: RAW_SIZE,
        language: state.language.lock().unwrap().clone(),
        languages: i18n::LANGUAGES
            .iter()
            .map(|language| LanguageInfo {
                code: language.code,
                name: language.native_name,
            })
            .collect(),
        dictionaries: i18n::LANGUAGES
            .iter()
            .map(|language| (language.code, i18n::dictionary(language.code)))
            .collect(),
        plan,
        check,
        payload_present: PAYLOAD_PRESENT,
        uninstall_dir: uninstall_target(env).map(|dir| dir.display().to_string()),
        data_dir: env.data_dir().display().to_string(),
    }
}

/// The window is ready: shown already painted, without a white flash.
#[tauri::command]
fn ready(window: tauri::WebviewWindow) {
    let _ = window.show();
    let _ = window.set_focus();
}

#[tauri::command]
fn set_language(state: tauri::State<'_, Shared>, window: tauri::WebviewWindow, code: String) {
    if let Some(code) = i18n::supported(&code) {
        *state.language.lock().unwrap() = code.to_string();
        let key = match state.mode {
            WindowMode::Install => "window.title",
            WindowMode::Uninstall => "window.titleUninstall",
        };
        let _ = window.set_title(&i18n::text(code, key));
    }
}

#[tauri::command]
fn check_dir(state: tauri::State<'_, Shared>, dir: String) -> DirCheck {
    install::check_dir(&state.env, &product(), &dir)
}

/// The system folder picker. Inside the chosen folder we install into a Phase subfolder.
#[tauri::command(async)]
fn pick_dir(
    state: tauri::State<'_, Shared>,
    window: tauri::WebviewWindow,
    current: String,
) -> Option<String> {
    let language = state.language.lock().unwrap().clone();
    let start = Path::new(&current)
        .ancestors()
        .find(|path| path.is_dir())
        .map(Path::to_path_buf);
    let mut dialog = rfd::FileDialog::new()
        .set_title(i18n::text(&language, "dir.pick"))
        .set_parent(&window);
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    let picked = dialog.pick_folder()?;
    let named_phase = picked
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("phase"));
    let dir = if named_phase {
        picked
    } else {
        picked.join("Phase")
    };
    Some(dir.display().to_string())
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum InstallEvent {
    Progress(Progress),
    #[serde(rename_all = "camelCase")]
    Done {
        dir: String,
    },
    #[serde(rename_all = "camelCase")]
    Failed {
        error: SetupError,
        key: String,
    },
}

fn failed_event(error: SetupError) -> InstallEvent {
    InstallEvent::Failed {
        key: error.key().to_string(),
        error,
    }
}

fn panic_error() -> SetupError {
    SetupError::Io {
        step: "internal".into(),
        detail: "internal error".into(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallArgs {
    dir: String,
    desktop: bool,
    autostart: bool,
}

#[tauri::command]
fn start_install(app: tauri::AppHandle, args: InstallArgs, channel: Channel<InstallEvent>) {
    let state = app.state::<Shared>();
    if state.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    state.cancel.store(false, Ordering::SeqCst);
    let language = state.language.lock().unwrap().clone();
    let handle = app.clone();
    std::thread::spawn(move || {
        let state = handle.state::<Shared>();
        let request = InstallRequest {
            dir: args.dir,
            desktop: args.desktop,
            autostart: args.autostart,
            language,
            language_explicit: true,
        };
        let result = catch_unwind(AssertUnwindSafe(|| {
            let progress_channel = channel.clone();
            install::install(
                &state.env,
                &product(),
                &request,
                &state.cancel,
                &mut move |progress| {
                    let _ = progress_channel.send(InstallEvent::Progress(progress));
                },
            )
        }))
        .unwrap_or_else(|_| Err(panic_error()));
        state.busy.store(false, Ordering::SeqCst);
        let event = match result {
            Ok(dir) => {
                *state.installed_exe.lock().unwrap() = Some(dir.join(APP_EXE));
                InstallEvent::Done {
                    dir: dir.display().to_string(),
                }
            }
            Err(error) => failed_event(error),
        };
        let _ = channel.send(event);
    });
}

#[tauri::command]
fn cancel(state: tauri::State<'_, Shared>) {
    state.cancel.store(true, Ordering::SeqCst);
}

#[derive(Serialize, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum UninstallEvent {
    #[serde(rename_all = "camelCase")]
    Done {
        data_deleted: bool,
        data_errors: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    Failed { error: SetupError, key: String },
}

#[tauri::command]
fn start_uninstall(app: tauri::AppHandle, delete_data: bool, channel: Channel<UninstallEvent>) {
    let state = app.state::<Shared>();
    if state.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    std::thread::spawn(move || {
        let state = handle.state::<Shared>();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let dir = uninstall_target(&state.env).ok_or(SetupError::NotInstalled)?;
            let outcome = uninstall::uninstall(
                &state.env,
                &UninstallRequest {
                    dir: dir.clone(),
                    delete_data,
                    running_exe: Some(own_exe()),
                },
            )?;
            // We start it right away, not on exit: the file gets removed even if the window is closed
            // abnormally. The step itself waits for this process to finish.
            if outcome.self_pending
                && uninstall::spawn_finish(
                    &state.env,
                    &own_exe(),
                    Some(&dir),
                    Some(&state.webview_dir),
                )
                .is_ok()
            {
                state.finish_started.store(true, Ordering::SeqCst);
            }
            Ok(outcome)
        }))
        .unwrap_or_else(|_| Err(panic_error()));
        state.busy.store(false, Ordering::SeqCst);
        let _ = channel.send(match result {
            Ok(outcome) => UninstallEvent::Done {
                data_deleted: delete_data,
                data_errors: outcome.data_errors,
            },
            Err(error) => UninstallEvent::Failed {
                key: error.key().to_string(),
                error,
            },
        });
    });
}

/// Whether a running Phase gets in the way. `dir` — the folder; without it all copies count (removing
/// data affects any of them).
#[tauri::command]
fn phase_running(state: tauri::State<'_, Shared>, dir: Option<String>) -> bool {
    !procs::blocking(&state.env, dir.as_deref().map(Path::new)).is_empty()
}

/// Politely asks Phase to close and waits. Never terminates by force.
#[tauri::command(async)]
fn close_phase(state: tauri::State<'_, Shared>, dir: Option<String>) -> QuitOutcome {
    state.cancel.store(false, Ordering::SeqCst);
    procs::close_gracefully(
        &state.env,
        dir.as_deref().map(Path::new),
        QUIT_WAIT,
        &state.cancel,
    )
}

#[tauri::command]
fn open_phase(state: tauri::State<'_, Shared>) -> Result<(), String> {
    let exe = state
        .installed_exe
        .lock()
        .unwrap()
        .clone()
        .ok_or("not installed")?;
    let mut command = std::process::Command::new(&exe);
    if let Some(dir) = exe.parent() {
        command.current_dir(dir);
    }
    // The installer's WebView2 folder and port must not go to Phase.
    command
        .env_remove("WEBVIEW2_USER_DATA_FOLDER")
        .env_remove("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS");
    #[cfg(debug_assertions)]
    if state.env.is_sandbox() {
        let root = state.env.process_scope.clone().unwrap_or_default();
        command
            .env("LOCALAPPDATA", &state.env.local_appdata)
            .env("WEBVIEW2_USER_DATA_FOLDER", root.join("app-webview"))
            .env("PHASE_SECOND_INSTANCE", "1");
    }
    command.spawn().map(|_| ()).map_err(|err| err.to_string())
}

#[tauri::command]
fn minimize_window(window: tauri::WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn close_window(window: tauri::WebviewWindow) {
    let _ = window.close();
}

fn run_window(env: SetupEnv, mode: WindowMode) -> i32 {
    let env = Arc::new(env);
    let language = existing::initial_language(&env);
    if !native::single_instance(&instance_name(&env)) {
        native::error(
            &i18n::text(language, "native.title"),
            &i18n::text(language, "error.alreadyRunning"),
        );
        return SetupError::AlreadyRunning.exit_code();
    }
    uninstall::sweep_cleanup_copies(&env);

    let webview_dir = env
        .temp
        .join(format!("{WEBVIEW_PREFIX}{}", std::process::id()));
    let title_key = match mode {
        WindowMode::Install => "window.title",
        WindowMode::Uninstall => "window.titleUninstall",
    };
    let title = i18n::text(language, title_key);
    let setup_dir = webview_dir.clone();

    let built = tauri::Builder::default()
        .manage(Shared {
            env: env.clone(),
            mode,
            language: Mutex::new(language.to_string()),
            cancel: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            webview_dir: webview_dir.clone(),
            finish_started: AtomicBool::new(false),
            installed_exe: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            boot,
            ready,
            set_language,
            check_dir,
            pick_dir,
            start_install,
            cancel,
            start_uninstall,
            phase_running,
            close_phase,
            open_phase,
            minimize_window,
            close_window,
        ])
        .on_window_event(|window, event| {
            // The close button or Alt+F4 during installation means cancel: the window closes
            // by itself at the user's decision after the rollback.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<Shared>();
                if state.busy.load(Ordering::SeqCst) {
                    api.prevent_close();
                    state.cancel.store(true, Ordering::SeqCst);
                }
            }
        })
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "setup", WebviewUrl::App("index.html".into()))
                .title(title.clone())
                .inner_size(880.0, 560.0)
                .resizable(false)
                .maximizable(false)
                .decorations(false)
                .center()
                .visible(false)
                .data_directory(setup_dir.clone())
                .build()?;
            Ok(())
        })
        .build(tauri::generate_context!());

    let app = match built {
        Ok(app) => app,
        Err(err) => {
            native::error(
                &i18n::text(language, "native.title"),
                &i18n::text(language, "native.windowFailed").replace("{detail}", &err.to_string()),
            );
            return 1;
        }
    };
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            let state = handle.state::<Shared>();
            if !state.finish_started.swap(true, Ordering::SeqCst) {
                let _ =
                    uninstall::spawn_finish(&state.env, &own_exe(), None, Some(&state.webview_dir));
            }
        }
    });
    0
}

/// WebView2 is needed by the window; if it is missing, with consent we install it from Microsoft's site.
fn ensure_webview(env: &SetupEnv) -> Result<(), i32> {
    if webview2::is_installed() {
        return Ok(());
    }
    let language = existing::initial_language(env);
    let title = i18n::text(language, "native.title");
    if !native::ask(&title, &i18n::text(language, "native.webviewAsk")) {
        native::error(&title, &i18n::text(language, "native.webviewDeclined"));
        return Err(EXIT_NO_WEBVIEW);
    }
    let failed = |detail: String| {
        native::error(
            &title,
            &i18n::text(language, "native.webviewFailed").replace("{detail}", &detail),
        );
        EXIT_NO_WEBVIEW
    };
    webview2::install_runtime(&env.temp).map_err(|err| failed(err.to_string()))?;
    if !webview2::is_installed() {
        return Err(failed("WebView2".to_string()));
    }
    Ok(())
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = own_exe();
    let exe_name = exe
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let silent_requested = args.iter().any(|arg| arg.eq_ignore_ascii_case("/S"));
    let command = match cli::parse(&args, &exe_name) {
        Ok(command) => command,
        Err(detail) => {
            let lang = i18n::system_language();
            let text = i18n::text(lang, "native.badArgs").replace("{detail}", &detail);
            if silent_requested {
                native::attach_console();
                eprintln!("{text}");
            } else {
                native::error(&i18n::text(lang, "native.title"), &text);
            }
            return EXIT_BAD_ARGS;
        }
    };
    if matches!(
        command,
        Command::VerifyPayload
            | Command::Install { silent: true, .. }
            | Command::Uninstall { silent: true, .. }
    ) {
        native::attach_console();
    }
    if matches!(command, Command::VerifyPayload) {
        return verify_payload();
    }
    let env = match env::detect() {
        Ok(env) => env,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    match command {
        Command::VerifyPayload => unreachable!("обработано выше"),
        Command::Finish {
            wait_pid,
            dir,
            webview,
        } => {
            uninstall::finish(&env, wait_pid, dir.as_deref(), webview.as_deref());
            0
        }
        Command::Install { silent: true, dir } => silent_install(&env, dir),
        Command::Uninstall {
            silent: true,
            delete_data,
        } => silent_uninstall(&env, delete_data),
        Command::Install { silent: false, .. } => match ensure_webview(&env) {
            Ok(()) => run_window(env, WindowMode::Install),
            Err(code) => code,
        },
        Command::Uninstall { silent: false, .. } => match ensure_webview(&env) {
            Ok(()) => run_window(env, WindowMode::Uninstall),
            Err(code) => code,
        },
    }
}
