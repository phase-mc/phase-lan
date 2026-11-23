//! Profiles: a separate device key (ID), in-game nickname and own list of allowed
//! devices.
//!
//! **Nickname** is the name friends see the player under (in the "let in?" question, in the
//! guest list). Stored in the profile folder in the [`NICKNAME_FILE`] file. Until the user
//! sets a nickname themselves, the program determines it automatically:
//!
//! 1. parameters of the running game (`--username`);
//! 2. launcher accounts and the log of the last game launch
//!    (see [`phase_minecraft::nickname`]);
//! 3. the Windows user name, and if there is none, the profile name.
//!
//! A nickname from the game (items 1–2) is saved to the profile at once. The Windows user name is not:
//! as soon as Minecraft appears on this PC, the program will pick up the real nickname.
//!
//! A nickname is a label, not proof: access to the room is granted only by the device ID.

use std::{cell::OnceCell, fs, io::ErrorKind, path::Path};

use anyhow::{Context, Result, bail};
use phase_lang::tr;
use phase_minecraft::nickname as game_nick;
use phase_tunnel::{access::AccessList, fsutil, identity, wire};
use serde::Serialize;

/// File with the list of allowed devices in the profile folder.
pub const ALLOW_FILE: &str = "allowed.txt";
/// File with the in-game nickname in the profile folder (one line, UTF-8).
pub const NICKNAME_FILE: &str = "nickname.txt";
/// Maximum nickname length.
pub const NICKNAME_MAX_CHARS: usize = wire::NICKNAME_MAX_CHARS;

/// Profile for the interface.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProfileInfo {
    pub name: String,
    /// In-game nickname (always present: set, found in the game or the OS user name).
    pub nickname: String,
    /// Device ID (`None` means the key is not created yet or unreadable, see `error`).
    pub device_id: Option<String>,
    /// How many devices are allowed to connect to this profile as a host.
    pub allowed_devices: usize,
    /// What is wrong with the profile (the key cannot be decrypted, the list is damaged).
    pub error: Option<String>,
}

/// Result of the nickname search in the game: one search per call, however many profiles there are.
type Detection = OnceCell<Option<String>>;

/// Existing profiles. Keys are **not created**: a profile without a key is shown without an ID.
/// A nickname found in the game is saved to the profile (see the module description).
pub fn list_profiles() -> Result<Vec<ProfileInfo>> {
    let root = identity::profiles_root()?;
    let detection = Detection::new();
    Ok(identity::list_profiles()?
        .into_iter()
        .map(|name| {
            let dir = root.join(&name);
            describe(name, &dir, &detection)
        })
        .collect())
}

fn describe(name: String, dir: &Path, detection: &Detection) -> ProfileInfo {
    let mut errors = Vec::new();
    let device_id = match identity::load_secret_key(dir) {
        Ok(key) => key.map(|k| k.public().to_string()),
        Err(err) => {
            errors.push(format!("{err:#}"));
            None
        }
    };
    let allowed_devices = match AccessList::load(dir.join(ALLOW_FILE)) {
        Ok(list) => list.len(),
        Err(err) => {
            errors.push(format!("{err:#}"));
            0
        }
    };
    ProfileInfo {
        nickname: resolve(dir, &name, detection),
        name,
        device_id,
        allowed_devices,
        error: (!errors.is_empty()).then(|| errors.join("; ")),
    }
}

/// Profile for the interface. If there is no key yet, creates it (and the profile folder).
pub fn ensure_profile(profile: &str) -> Result<ProfileInfo> {
    let dir = identity::profile_dir(profile)?;
    identity::load_or_create_secret_key(&dir)?;
    Ok(describe(profile.to_string(), &dir, &Detection::new()))
}

/// Device ID of the profile. If there is no key yet, creates it (and the profile folder).
pub fn device_id(profile: &str) -> Result<String> {
    let dir = identity::profile_dir(profile)?;
    Ok(identity::load_or_create_secret_key(&dir)?
        .public()
        .to_string())
}

/// Validates a nickname typed by the user: removes control and invisible characters
/// and extra spaces; the result is 1 to [`NICKNAME_MAX_CHARS`] characters. A nickname that is too long is
/// an error, not a silent truncation.
pub fn validate_nickname(raw: &str) -> Result<String> {
    let nickname = wire::normalize_nickname(raw);
    let len = nickname.chars().count();
    if len == 0 {
        bail!(tr!(
            "ник не может быть пустым",
            "the nickname can't be empty"
        ));
    }
    if len > NICKNAME_MAX_CHARS {
        bail!(tr!(
            format!("ник не длиннее {NICKNAME_MAX_CHARS} знаков (сейчас {len})"),
            format!("the nickname can be at most {NICKNAME_MAX_CHARS} characters (now {len})")
        ));
    }
    Ok(nickname)
}

/// The profile's in-game nickname (see the module description). The call blocks.
pub fn nickname(profile: &str) -> Result<String> {
    let dir = identity::profile_dir(profile)?;
    Ok(resolve_nickname(&dir, profile))
}

/// The profile's in-game nickname in folder `dir`: the saved one, otherwise found in the game (and
/// saved right away), otherwise the OS user name or the profile name. The call blocks.
pub fn resolve_nickname(dir: &Path, profile: &str) -> String {
    resolve(dir, profile, &Detection::new())
}

fn resolve(dir: &Path, profile: &str, detection: &Detection) -> String {
    if let Some(saved) = saved_nickname(dir) {
        return saved;
    }
    let detected = detection.get_or_init(|| {
        game_nick::detect().and_then(|found| {
            let nickname = wire::sanitize_nickname(&found.nickname)?;
            tracing::info!(
                "игровой ник определён автоматически: {nickname} ({})",
                found.source.describe()
            );
            Some(nickname)
        })
    });
    match detected {
        Some(nickname) => {
            // The profile folder may not exist yet: then the nickname will be saved when it is created.
            if dir.is_dir()
                && let Err(err) = save_nickname(dir, nickname)
            {
                tracing::warn!("не удалось сохранить ник в профиль: {err:#}");
            }
            nickname.clone()
        }
        None => system_nickname(profile),
    }
}

/// Fallback nickname: the OS user name, and if there is none, the profile name.
fn system_nickname(profile: &str) -> String {
    ["USERNAME", "USER"]
        .iter()
        .filter_map(|var| std::env::var(var).ok())
        .find_map(|name| wire::sanitize_nickname(&name))
        .or_else(|| wire::sanitize_nickname(profile))
        .unwrap_or_else(|| "Player".to_string())
}

/// Nickname from the profile file. `None` means the file is missing, empty or unreadable (then the nickname
/// is determined again). The contents are cleaned: the file could have been edited by hand.
fn saved_nickname(dir: &Path) -> Option<String> {
    let file = dir.join(NICKNAME_FILE);
    match fs::read_to_string(&file) {
        Ok(text) => wire::sanitize_nickname(text.trim_start_matches('\u{feff}')),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!("не удалось прочитать {}: {err}", file.display());
            None
        }
    }
}

fn save_nickname(dir: &Path, nickname: &str) -> Result<()> {
    let file = dir.join(NICKNAME_FILE);
    fsutil::write_atomic(&file, format!("{nickname}\n").as_bytes()).with_context(|| {
        tr!(
            format!("не удалось сохранить ник в {}", file.display()),
            format!("could not save the nickname to {}", file.display())
        )
    })
}

/// Sets the profile's nickname (after validation, see [`validate_nickname`]). If there is no
/// profile yet, creates it. Returns the updated profile.
pub fn set_nickname(profile: &str, nickname: &str) -> Result<ProfileInfo> {
    let dir = identity::profile_dir(profile)?;
    set_nickname_in(&dir, nickname)?;
    identity::load_or_create_secret_key(&dir)?;
    Ok(describe(profile.to_string(), &dir, &Detection::new()))
}

/// Sets the profile's nickname in folder `dir`. Returns the nickname as it was saved.
pub fn set_nickname_in(dir: &Path, nickname: &str) -> Result<String> {
    let nickname = validate_nickname(nickname)?;
    save_nickname(dir, &nickname)?;
    Ok(nickname)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phase-profile-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn nickname_is_validated() {
        assert_eq!(
            validate_nickname("  GrayIsko1337 ").unwrap(),
            "GrayIsko1337"
        );
        assert_eq!(validate_nickname("Серый\t  Волк").unwrap(), "Серый Волк");
        // Control and invisible characters are removed, not treated as an error.
        assert_eq!(validate_nickname("St\u{1b}eve\u{200b}").unwrap(), "Steve");
        assert_eq!(validate_nickname(&"x".repeat(24)).unwrap().len(), 24);

        let empty = validate_nickname(" \t\u{feff} ").unwrap_err();
        assert!(format!("{empty:#}").contains("пустым"), "{empty:#}");
        let long = validate_nickname(&"x".repeat(25)).unwrap_err();
        assert!(format!("{long:#}").contains("24"), "{long:#}");
    }

    #[test]
    fn saved_nickname_wins_and_survives_reload() {
        let dir = temp_dir("saved");
        assert_eq!(set_nickname_in(&dir, "  Alex_01 ").unwrap(), "Alex_01");
        // The saved nickname does not depend on what is found in the game on this PC.
        assert_eq!(resolve_nickname(&dir, "default"), "Alex_01");
        assert_eq!(
            fs::read_to_string(dir.join(NICKNAME_FILE)).unwrap(),
            "Alex_01\n"
        );
        // A bad nickname does not overwrite a good one.
        assert!(set_nickname_in(&dir, "   ").is_err());
        assert_eq!(resolve_nickname(&dir, "default"), "Alex_01");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hand_edited_nickname_file_is_cleaned() {
        let dir = temp_dir("edited");
        // Windows Notepad: byte order mark, CRLF line ending, extra spaces.
        fs::write(dir.join(NICKNAME_FILE), "\u{feff}  Steve\u{7}  \r\n").unwrap();
        assert_eq!(resolve_nickname(&dir, "default"), "Steve");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_saved_nickname_there_is_always_some_name() {
        let dir = temp_dir("auto");
        // What exactly is found depends on the PC (game, launchers, user name), but a nickname
        // is always there and always valid.
        let nickname = resolve_nickname(&dir, "default");
        assert_eq!(validate_nickname(&nickname).unwrap(), nickname);
        assert!(!system_nickname("default").is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
