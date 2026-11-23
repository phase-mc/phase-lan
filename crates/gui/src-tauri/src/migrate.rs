//! Migrating window data from the old program name (LanBridge → Phase).
//!
//! WebView2 keeps window data in `%LOCALAPPDATA%\<identifier>`. Along with the identifier
//! (`dev.lanbridge.desktop` → `app.phase.desktop`) the folder changed too, and with it the window
//! settings from `localStorage` would be lost: theme, profile, network, mods folder. So before the
//! window starts we carry `localStorage` over from the old folder. WebView2 caches (about 100 MB) are not
//! copied: the browser will rebuild them.
//!
//! The migration happens once: when the new folder doesn't exist yet. An error is logged and
//! doesn't interrupt startup — the window just opens with default settings.
//! Profile data (device key, friends) is migrated by `identity::data_dir`.

use std::{fs, io, path::Path};

/// The window identifier before the rename.
const LEGACY_IDENTIFIER: &str = "dev.lanbridge.desktop";
/// Where WebView2 keeps `localStorage` inside the window data folder.
const LOCAL_STORAGE: [&str; 3] = ["EBWebView", "Default", "Local Storage"];
/// The leveldb lock file: a running old version holds it open, and the new one
/// creates its own.
const LOCK_FILE: &str = "LOCK";

/// Carries the window's `localStorage` over from the old identifier's folder to the `identifier` folder.
pub fn webview_data(identifier: &str) {
    // Our own WebView2 folder (developer checks) is left alone.
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_some() || identifier == LEGACY_IDENTIFIER {
        return;
    }
    // The same function Tauri uses to pick the window data folder.
    let Some(base) = dirs::data_local_dir() else {
        return;
    };
    let target = base.join(identifier);
    let from = LOCAL_STORAGE
        .iter()
        .fold(base.join(LEGACY_IDENTIFIER), |path, part| path.join(part));
    if target.exists() || !from.is_dir() {
        return;
    }
    // First into a temporary folder, then with a single rename: an interrupted copy
    // won't leave a half-empty folder that would stop the migration from ever repeating.
    let part = base.join(format!("{identifier}.part"));
    let _ = fs::remove_dir_all(&part);
    let to = LOCAL_STORAGE
        .iter()
        .fold(part.clone(), |path, piece| path.join(piece));
    match copy_dir(&from, &to).and_then(|()| fs::rename(&part, &target)) {
        Ok(()) => tracing::info!("настройки окна перенесены из {}", from.display()),
        Err(err) => {
            let _ = fs::remove_dir_all(&part);
            tracing::warn!(
                "не удалось перенести настройки окна из {}: {err}",
                from.display()
            );
        }
    }
}

/// Copies a folder entirely, except the leveldb lock file.
fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&entry.path(), &to.join(&name))?;
        } else if kind.is_file() && name != LOCK_FILE {
            fs::copy(entry.path(), to.join(&name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_everything_but_the_lock_file() {
        let root = std::env::temp_dir().join(format!("phase-migrate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let from = root.join("from");
        fs::create_dir_all(from.join("leveldb")).unwrap();
        fs::write(from.join("leveldb").join("000003.log"), b"settings").unwrap();
        fs::write(from.join("leveldb").join(LOCK_FILE), b"").unwrap();

        copy_dir(&from, &root.join("to")).unwrap();
        let copied = root.join("to").join("leveldb");
        assert_eq!(fs::read(copied.join("000003.log")).unwrap(), b"settings");
        assert!(!copied.join(LOCK_FILE).exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
