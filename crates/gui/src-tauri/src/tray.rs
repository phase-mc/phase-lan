//! Phase in the Windows tray.
//!
//! - The window's close button doesn't quit the program but hides the window to the tray: the room keeps
//!   running, the auto-room (`autoroom.rs`) waits for an open world. The first time there is a short
//!   notification "Phase is running in the tray".
//! - Icon menu: "Open Phase" and "Quit". Clicking the icon opens the window.
//! - The icon follows the taskbar theme (spec §2.5): a light mark on a dark taskbar,
//!   a dark one on a light taskbar; it changes on the fly.
//! - "Launch with Windows" — the autostart plugin (the current user's `Run` key)
//!   with the [`AUTOSTART_FLAG`] flag: a window launched this way goes straight to the tray.

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use phase_core::SessionEvent;
use tauri::{
    AppHandle, Manager, Runtime, Window, WindowEvent,
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt as _;

use crate::links::show_main_window;

/// The argument Windows launches Phase with at sign-in.
pub const AUTOSTART_FLAG: &str = "--autostart";
const TRAY_ID: &str = "phase";
/// How often to check the taskbar theme.
const THEME_POLL: Duration = Duration::from_secs(5);
/// Marker file "the tray hint has already been shown" in the data folder.
const HINT_MARK: &str = "tray-hint-shown";

/// The user chose "Quit": closing the window is no longer intercepted.
static QUITTING: AtomicBool = AtomicBool::new(false);

/// Whether Phase was launched with Windows (then the window goes straight to the tray).
pub fn started_by_autostart() -> bool {
    std::env::args().any(|arg| arg == AUTOSTART_FLAG)
}

/// The tray icon with a menu.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let menu = tray_menu(app)?;
    let mut light = taskbar_is_light();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(tray_icon(light)?)
        .tooltip("Phase")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "quit" => quit_app(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    // The taskbar theme changes without a restart — so does the icon.
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(THEME_POLL).await;
            let now = taskbar_is_light();
            if now == light {
                continue;
            }
            light = now;
            if let (Some(tray), Ok(icon)) = (handle.tray_by_id(TRAY_ID), tray_icon(light)) {
                let _ = tray.set_icon(Some(icon));
            }
        }
    });
    Ok(())
}

/// The icon menu in the current language.
fn tray_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let open = MenuItem::with_id(
        app,
        "open",
        phase_lang::tr!("Открыть Phase", "Open Phase"),
        true,
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(
        app,
        "quit",
        phase_lang::tr!("Выйти", "Quit"),
        true,
        None::<&str>,
    )?;
    Menu::with_items(app, &[&open, &quit])
}

/// The language changed: the icon menu is rebuilt on the fly.
pub fn refresh_language(app: &AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), tray_menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn tray_icon(light_taskbar: bool) -> tauri::Result<Image<'static>> {
    let bytes: &[u8] = if light_taskbar {
        include_bytes!("../icons/tray-light.ico")
    } else {
        include_bytes!("../icons/tray-dark.ico")
    };
    Image::from_bytes(bytes)
}

/// "Quit": a real exit (the session is stopped in `RunEvent::Exit`).
pub fn quit_app<R: Runtime>(app: &AppHandle<R>) {
    QUITTING.store(true, Ordering::SeqCst);
    app.exit(0);
}

/// Window close button: hide to the tray instead of quitting.
pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event {
        if QUITTING.load(Ordering::SeqCst) {
            return;
        }
        api.prevent_close();
        let _ = window.hide();
        first_hide_hint(window.app_handle());
    }
}

/// The "Phase is running in the tray" notification — once per computer.
fn first_hide_hint(app: &AppHandle) {
    let Ok(dir) = phase_tunnel::identity::data_dir() else {
        return;
    };
    let mark = dir.join(HINT_MARK);
    if mark.exists() {
        return;
    }
    notify(
        app,
        phase_lang::tr!("Phase работает в трее", "Phase is running in the tray"),
        phase_lang::tr!(
            "Комната не закрылась. Открыть окно или выйти — значок Phase рядом с часами.",
            "Your room is still open. To show the window or quit, use the Phase icon near the clock."
        ),
    );
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(mark, b"1");
}

/// A Windows system notification. An error (notifications turned off) goes to the log only.
pub fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(err) = app.notification().builder().title(title).body(body).show() {
        tracing::debug!("уведомление не показано: {err}");
    }
}

/// The window is hidden (in the tray or minimized).
fn window_hidden(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .is_none_or(|w| !w.is_visible().unwrap_or(true) || w.is_minimized().unwrap_or(false))
}

/// Core events worth mentioning while the window is in the tray: a guest is waiting for "Let them in?".
pub fn on_core_event(app: &AppHandle, event: &SessionEvent) {
    if let SessionEvent::ApprovalRequested { nickname, .. } = event
        && window_hidden(app)
    {
        let who = nickname
            .as_deref()
            .unwrap_or(phase_lang::tr!("Новый игрок", "A new player"));
        notify(
            app,
            &phase_lang::tr!(
                format!("{who} хочет зайти в мир"),
                format!("{who} wants to join your world")
            ),
            phase_lang::tr!(
                "Откройте Phase из трея, чтобы ответить «Пустить?».",
                "Open Phase from the tray to answer “Let them in?”."
            ),
        );
    }
}

// ---------------------------------------------------------------- autostart

/// Whether launch with Windows is on.
#[tauri::command]
pub fn autostart_enabled(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// Turn launch with Windows on or off. Returns the resulting state.
#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|err| {
        phase_lang::tr!(
            format!("не удалось изменить автозапуск: {err}"),
            format!("could not change autostart: {err}")
        )
    })?;
    Ok(manager.is_enabled().unwrap_or(enabled))
}

/// Whether the icon needs a dark mark (light taskbar). If it can't be read — a dark taskbar.
#[cfg(windows)]
fn taskbar_is_light() -> bool {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("SystemUsesLightTheme");
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated, the buffer is a u32 of the right size.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    status == 0 && data == 1
}

#[cfg(not(windows))]
fn taskbar_is_light() -> bool {
    false
}
