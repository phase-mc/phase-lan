//! Program language: the window chooses it and tells the core, the core speaks it (tray,
//! notifications, session messages, in-game captions).
//!
//! Order of choice: setting → installer choice (`HKCU\Software\Phase`, `Language`) →
//! Windows language. The window stores the setting (localStorage with the rest); a copy lies in
//! the data folder so the tray and auto-room speak it before the window loads.

use phase_lang::Lang;
use serde::Serialize;
use tauri::AppHandle;

/// A copy of the language setting for launching without a window (the tray on autostart).
const LANG_FILE: &str = "language";

/// What is known about the language before the user chooses.
#[derive(Serialize)]
pub struct LanguageInfo {
    /// Installer choice: `ru` / `en` (`None` — the installer wrote nothing).
    installer: Option<String>,
    /// Windows UI language: `ru` / `en`.
    system: String,
}

/// Language at program start, before the window loads.
pub fn init() {
    let stored = phase_tunnel::identity::data_dir()
        .ok()
        .and_then(|dir| std::fs::read_to_string(dir.join(LANG_FILE)).ok());
    let lang = phase_lang::detect(stored.as_deref());
    phase_lang::set(lang);
    tracing::info!("язык: {}", lang.code());
}

/// Installer choice and Windows language: the window decides by them while it has no setting of its own.
#[tauri::command]
pub fn language_info() -> LanguageInfo {
    LanguageInfo {
        installer: phase_lang::installer_choice().map(|lang| lang.code().to_string()),
        system: phase_lang::os_language_tag().unwrap_or_else(|| "en".to_string()),
    }
}

/// The window reports the language (at startup and on change): the core switches to it immediately.
#[tauri::command]
pub fn set_language(app: AppHandle, code: String) -> Result<String, String> {
    let lang = Lang::from_code(&code).ok_or_else(|| format!("unknown language “{code}”"))?;
    if lang != phase_lang::current() {
        phase_lang::set(lang);
        crate::tray::refresh_language(&app);
    }
    if let Ok(dir) = phase_tunnel::identity::data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(LANG_FILE), lang.code());
    }
    Ok(lang.code().to_string())
}
