//! The Phase installer for Windows: all the logic without a window.
//!
//! The rules this code was written for:
//!
//! - the installer owns only the files from its manifest (`phase-install.json`);
//!   it never touches user data in `%LOCALAPPDATA%\Phase`, except with explicit
//!   consent on uninstall;
//! - any error or cancellation rolls back everything already done (the [`tx::Tx`] journal);
//! - a running Phase is never terminated by force: only a polite request;
//! - no packers, hidden actions or network (the network is needed only to install WebView2
//!   if it is missing, and only with the user's consent).

pub mod cli;
pub mod env;
pub mod error;
pub mod existing;
pub mod i18n;
pub mod install;
pub mod integration;
pub mod manifest;
pub mod paths;
pub mod payload;
pub mod procs;
pub mod reg;
pub mod shortcut;
pub mod tx;
pub mod uninstall;
pub mod webview2;

/// The program name in files, the registry and shortcuts.
pub const PRODUCT: &str = "Phase";
/// The publisher in "Installed apps".
pub const PUBLISHER: &str = "Phase";
/// The program file in the install folder.
pub const APP_EXE: &str = "Phase.exe";
/// The uninstaller: the same name as the NSIS installer's, so an update over it
/// replaces the file rather than leaving a second one.
pub const UNINSTALL_EXE: &str = "uninstall.exe";
/// The manifest of installed files.
pub const MANIFEST_FILE: &str = "phase-install.json";
/// The Phase window identifier: the WebView2 folder and the AppUserModelID of shortcuts.
pub const APP_IDENTIFIER: &str = "app.phase.desktop";
/// The Phase data folder inside `%LOCALAPPDATA%`.
pub const DATA_DIR_NAME: &str = "Phase";
/// The autostart argument — the same as `tray::AUTOSTART_FLAG` in the Phase window.
pub const AUTOSTART_FLAG: &str = "--autostart";
