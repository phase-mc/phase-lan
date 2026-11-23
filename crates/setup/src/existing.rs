//! What is already installed and what to offer the user.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{
    APP_EXE, PRODUCT, UNINSTALL_EXE,
    env::SetupEnv,
    i18n,
    integration::{self, LANGUAGE_VALUE, PRODUCT_KEY, UNINSTALL_KEY, VENDOR_KEY},
    manifest::Manifest,
    paths, shortcut,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallKind {
    /// Installed by this installer: there is a manifest.
    Managed,
    /// Installed by the previous NSIS installer.
    Nsis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Existing {
    pub dir: PathBuf,
    pub version: Option<String>,
    pub kind: InstallKind,
}

/// Finds an installation by its registry entry. An entry without files is considered stale.
pub fn detect(env: &SetupEnv) -> Option<Existing> {
    let location = env
        .reg
        .get_str(UNINSTALL_KEY, "InstallLocation")
        .or_else(|| env.reg.get_str(PRODUCT_KEY, ""))?;
    let location = integration::unquoted(&location);
    if location.is_empty() {
        return None;
    }
    let dir = PathBuf::from(location);
    if !dir.join(APP_EXE).is_file() {
        return None;
    }
    let managed = matches!(Manifest::load(&dir), Ok(Some(_)));
    if !managed && !dir.join(UNINSTALL_EXE).is_file() {
        return None;
    }
    Some(Existing {
        version: env.reg.get_str(UNINSTALL_KEY, "DisplayVersion"),
        kind: if managed {
            InstallKind::Managed
        } else {
            InstallKind::Nsis
        },
        dir,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallMode {
    Fresh,
    Update,
    Reinstall,
    Downgrade,
}

/// Parses `1.2.3` for comparison. Non-numeric parts count as zero.
fn version_key(version: &str) -> [u64; 3] {
    let mut parts = version
        .split(['.', '-', '+'])
        .map(|part| part.parse().unwrap_or(0));
    [
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    ]
}

pub fn mode_for(existing: Option<&Existing>, version: &str) -> InstallMode {
    let Some(existing) = existing else {
        return InstallMode::Fresh;
    };
    let Some(current) = &existing.version else {
        return InstallMode::Update;
    };
    match version_key(current).cmp(&version_key(version)) {
        std::cmp::Ordering::Less => InstallMode::Update,
        std::cmp::Ordering::Equal => InstallMode::Reinstall,
        std::cmp::Ordering::Greater => InstallMode::Downgrade,
    }
}

/// Values of the first screen (and of a silent install).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub mode: InstallMode,
    pub dir: PathBuf,
    /// On an update the folder doesn't change: otherwise two copies would remain.
    pub dir_locked: bool,
    pub desktop: bool,
    pub autostart: bool,
    pub existing: Option<Existing>,
}

pub fn desktop_link(env: &SetupEnv) -> PathBuf {
    env.desktop.join(format!("{PRODUCT}.lnk"))
}

pub fn start_menu_link(env: &SetupEnv) -> PathBuf {
    env.start_menu.join(format!("{PRODUCT}.lnk"))
}

pub fn plan(env: &SetupEnv, version: &str) -> Plan {
    let existing = detect(env);
    let mode = mode_for(existing.as_ref(), version);
    match &existing {
        Some(found) => {
            let exe = found.dir.join(APP_EXE);
            Plan {
                mode,
                dir: found.dir.clone(),
                dir_locked: true,
                // On an update we impose nothing: what is now stays as it is.
                desktop: shortcut::points_to(&desktop_link(env), &exe),
                autostart: integration::autostart_enabled(env.reg.as_ref(), &exe),
                existing,
            }
        }
        None => Plan {
            mode,
            dir: env.default_install_dir(),
            dir_locked: false,
            desktop: true,
            autostart: false,
            existing: None,
        },
    }
}

/// The saved language choice, if it is supported.
pub fn saved_language(env: &SetupEnv) -> Option<&'static str> {
    env.reg
        .get_str(VENDOR_KEY, LANGUAGE_VALUE)
        .and_then(|code| i18n::supported(&code))
}

/// Default language: the saved one, otherwise the Windows language, otherwise English.
pub fn initial_language(env: &SetupEnv) -> &'static str {
    saved_language(env).unwrap_or_else(i18n::system_language)
}

/// The installation the uninstaller `exe` belongs to.
pub fn install_dir_of_uninstaller(env: &SetupEnv, exe: &Path) -> Option<PathBuf> {
    let beside = exe.parent()?;
    if beside.join(crate::MANIFEST_FILE).is_file() || beside.join(APP_EXE).is_file() {
        return Some(beside.to_path_buf());
    }
    detect(env)
        .map(|found| found.dir)
        .filter(|dir| paths::same_path(dir, beside))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(version: Option<&str>) -> Existing {
        Existing {
            dir: PathBuf::from(r"C:\P"),
            version: version.map(str::to_string),
            kind: InstallKind::Nsis,
        }
    }

    #[test]
    fn modes_follow_versions() {
        assert_eq!(mode_for(None, "1.0.0"), InstallMode::Fresh);
        assert_eq!(
            mode_for(Some(&found(Some("0.1.0"))), "1.0.0"),
            InstallMode::Update
        );
        assert_eq!(
            mode_for(Some(&found(Some("1.0.0"))), "1.0.0"),
            InstallMode::Reinstall
        );
        assert_eq!(
            mode_for(Some(&found(Some("1.10.0"))), "1.9.0"),
            InstallMode::Downgrade
        );
        assert_eq!(mode_for(Some(&found(None)), "1.0.0"), InstallMode::Update);
    }
}
