//! The user's in-game nickname on this PC, so friends see a familiar name rather than the device
//! ID or the profile name.
//!
//! Where we look, in order:
//!
//! 1. **Running game.** The Minecraft process parameters contain `--username <nickname>`: this is
//!    the nickname the user is playing under right now. Works with any launcher.
//! 2. **Launcher files and game logs**, the most recently modified one:
//!    - Minecraft Launcher: `%APPDATA%\.minecraft\launcher_accounts*.json` and the old
//!      `launcher_profiles.json` (before 2020 accounts were stored in it);
//!    - Lunar Client: `%USERPROFILE%\.lunarclient\settings\game\accounts.json`;
//!    - Prism Launcher: `%APPDATA%\PrismLauncher\accounts.json`;
//!    - TLauncher: `%APPDATA%\.minecraft\TlauncherProfiles.json`;
//!    - game log `logs\latest.log` in the game folders of all known launchers (see
//!      [`crate::voice::known_game_dirs`]): the line `Setting user: <nickname>`. This is how the
//!      nickname is found for Modrinth App too, which keeps accounts in a database, not in JSON.
//!
//! If nothing is found, the calling code picks the nickname (the Windows user name).
//!
//! Only names are read. Access tokens from the same files and from the game command line
//! are never passed anywhere and never written to the log.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde_json::Value;

use crate::voice;

/// Maximum length of a nickname that counts as an in-game one.
pub const MAX_CHARS: usize = 24;

/// How much to read from the start of the game log: the nickname is written at launch, in the first lines.
const LOG_HEAD: u64 = 256 * 1024;

/// Executable file names of the running game (Java).
pub(crate) const GAME_EXE: &[&str] = &["javaw.exe", "java.exe"];

/// The command line belongs to the Minecraft game (client), not to another Java program:
/// it has parameters that launchers pass only to the game.
pub fn is_game_command_line(command_line: &str) -> bool {
    ["--gameDir", "--assetsDir", "--accessToken", "--assetIndex"]
        .iter()
        .any(|flag| command_line.contains(flag))
}

/// Where the nickname came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Parameters of the running game.
    GameProcess,
    /// Launcher account file (launcher name).
    Launcher(&'static str),
    /// Log of the last game launch.
    GameLog,
}

impl Source {
    /// Where the nickname came from, in words, for the log.
    pub fn describe(&self) -> String {
        match self {
            Self::GameProcess => {
                phase_lang::tr!("из запущенной игры", "from the running game").to_string()
            }
            Self::Launcher(name) => phase_lang::tr!(
                format!("из учётной записи {name}"),
                format!("from the {name} account")
            ),
            Self::GameLog => phase_lang::tr!(
                "из журнала последнего запуска игры",
                "from the log of the last game launch"
            )
            .to_string(),
        }
    }
}

/// The nickname that was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub nickname: String,
    pub source: Source,
}

/// Looks for the in-game nickname on this PC (see the module description). The call blocks: it reads the list
/// of processes and a few small files.
pub fn detect() -> Option<Detected> {
    from_running_game().or_else(from_files)
}

fn from_running_game() -> Option<Detected> {
    let processes = crate::game_process::game_processes().ok()?;
    processes
        .iter()
        .find_map(|p| from_command_line(&p.command_line))
        .map(|nickname| Detected {
            nickname,
            source: Source::GameProcess,
        })
}

/// The most recent nickname from launcher files and game logs.
fn from_files() -> Option<Detected> {
    let mut found: Vec<(SystemTime, Detected)> = Vec::new();
    let mut add = |file: PathBuf, source: Source, parse: fn(&str) -> Option<String>| {
        let Ok(modified) = fs::metadata(&file).and_then(|m| m.modified()) else {
            return;
        };
        let text = match source {
            Source::GameLog => read_head(&file, LOG_HEAD),
            _ => fs::read_to_string(&file).ok(),
        };
        if let Some(nickname) = text.as_deref().and_then(parse) {
            found.push((modified, Detected { nickname, source }));
        }
    };

    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        let official = appdata.join(".minecraft");
        for name in [
            "launcher_accounts_microsoft_store.json",
            "launcher_accounts.json",
        ] {
            add(
                official.join(name),
                Source::Launcher("Minecraft Launcher"),
                from_accounts_json,
            );
        }
        add(
            official.join("launcher_profiles.json"),
            Source::Launcher("Minecraft Launcher"),
            from_legacy_profiles_json,
        );
        add(
            official.join("TlauncherProfiles.json"),
            Source::Launcher("TLauncher"),
            from_accounts_json,
        );
        add(
            appdata.join("PrismLauncher").join("accounts.json"),
            Source::Launcher("Prism Launcher"),
            from_accounts_json,
        );
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        add(
            profile
                .join(".lunarclient")
                .join("settings")
                .join("game")
                .join("accounts.json"),
            Source::Launcher("Lunar Client"),
            from_accounts_json,
        );
    }
    for dir in voice::known_game_dirs() {
        add(
            dir.join("logs").join("latest.log"),
            Source::GameLog,
            from_log,
        );
    }

    found
        .into_iter()
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, detected)| detected)
}

/// Start of a file as text (a game log can be large and not UTF-8).
fn read_head(file: &Path, limit: u64) -> Option<String> {
    let mut bytes = Vec::new();
    fs::File::open(file)
        .ok()?
        .take(limit)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Checks that a string looks like an in-game nickname: no spaces or control characters,
/// not longer than [`MAX_CHARS`] and not an email address (in launcher files the `username` field
/// may hold the account email, which must never become a nickname).
fn game_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    let ok = !name.is_empty()
        && name.chars().count() <= MAX_CHARS
        && !name.contains('@')
        && !name.chars().any(|c| c.is_control() || c.is_whitespace());
    ok.then(|| name.to_string())
}

/// Nickname from the running game's command line: the value of the `--username` parameter.
/// Command lines of other Java programs do not fit: the game's own parameters are needed too.
pub fn from_command_line(command_line: &str) -> Option<String> {
    const FLAG: &str = "--username";
    if !is_game_command_line(command_line) {
        return None;
    }
    let mut rest = command_line;
    while let Some(at) = rest.find(FLAG) {
        let before_ok = rest[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || c == '"');
        let after = &rest[at + FLAG.len()..];
        rest = after;
        // The whole parameter: followed by a space (`--username Name`) or an equals sign.
        let Some(value) = after
            .strip_prefix('=')
            .or_else(|| after.starts_with(char::is_whitespace).then_some(after))
        else {
            continue;
        };
        if !before_ok {
            continue;
        }
        let value = value.trim_start();
        let name = match value.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or(""),
            None => value.split(char::is_whitespace).next().unwrap_or(""),
        };
        if let Some(name) = game_name(name) {
            return Some(name);
        }
    }
    None
}

/// Nickname from a launcher account file. Understands the common shape of such files: an
/// `accounts` list (object by identifier or array), a mark for the active account and the name in
/// `minecraftProfile.name`, `profile.name`, `displayName` or `username`.
pub fn from_accounts_json(text: &str) -> Option<String> {
    let root: Value = serde_json::from_str(text).ok()?;
    let accounts = root.get("accounts")?;
    let active_id = [
        "activeAccountLocalId",
        "selectedAccountUUID",
        "freeAccountUUID",
    ]
    .iter()
    .find_map(|key| root.get(key)?.as_str());

    // The active account first, then the rest in order.
    let mut entries: Vec<&Value> = Vec::new();
    match accounts {
        Value::Object(map) => {
            entries.extend(active_id.and_then(|id| map.get(id)));
            entries.extend(map.values());
        }
        Value::Array(list) => {
            let is_active =
                |entry: &&Value| entry.get("active").and_then(Value::as_bool) == Some(true);
            entries.extend(list.iter().filter(is_active));
            entries.extend(list.iter());
        }
        _ => return None,
    }
    entries.into_iter().find_map(account_name)
}

fn account_name(account: &Value) -> Option<String> {
    [
        account.pointer("/minecraftProfile/name"),
        account.pointer("/profile/name"),
        account.get("displayName"),
        account.get("username"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .find_map(game_name)
}

/// Nickname from the old `launcher_profiles.json` (before 2020): the
/// `authenticationDatabase` section, the selected entry is in `selectedUser`.
pub fn from_legacy_profiles_json(text: &str) -> Option<String> {
    let root: Value = serde_json::from_str(text).ok()?;
    let database = root.get("authenticationDatabase")?.as_object()?;
    let selected = root
        .pointer("/selectedUser/account")
        .or_else(|| root.get("selectedUser"))
        .and_then(Value::as_str);
    let mut entries: Vec<&Value> = Vec::new();
    entries.extend(selected.and_then(|id| database.get(id)));
    entries.extend(database.values());
    entries.into_iter().find_map(|entry| {
        let own = entry.get("displayName").and_then(Value::as_str);
        let profiles = entry
            .get("profiles")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|profiles| profiles.values())
            .filter_map(|profile| profile.get("displayName")?.as_str());
        own.into_iter().chain(profiles).find_map(game_name)
    })
}

/// Nickname from the game log: the line `Setting user: <nickname>` (the last one if there are several).
pub fn from_log(text: &str) -> Option<String> {
    const MARK: &str = "Setting user: ";
    text.lines()
        .filter_map(|line| line.split_once(MARK))
        .filter_map(|(_, name)| game_name(name))
        .next_back()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nickname_from_game_command_line() {
        // This is how the official launcher starts the game (abridged).
        let official = r#""C:\Program Files\Java\bin\javaw.exe" -Xmx2G -cp C:\libs\a.jar net.minecraft.client.main.Main --username GrayIsko1337 --version 1.21.1 --gameDir C:\Users\u\AppData\Roaming\.minecraft --assetsDir C:\assets --accessToken eyJ.secret --userType msa"#;
        assert_eq!(from_command_line(official).as_deref(), Some("GrayIsko1337"));

        // Nickname in quotes and with an equals sign.
        let quoted = r#"javaw.exe --gameDir "D:\My Games\mc" --username "Alex_01" --accessToken 0"#;
        assert_eq!(from_command_line(quoted).as_deref(), Some("Alex_01"));
        let equals = "java --assetIndex 17 --username=Steve --accessToken 0";
        assert_eq!(from_command_line(equals).as_deref(), Some("Steve"));
    }

    #[test]
    fn other_java_programs_are_not_the_game() {
        // A server or any other Java program with a similar parameter.
        assert_eq!(
            from_command_line("java -jar tool.jar --username admin"),
            None
        );
        assert_eq!(from_command_line("javaw.exe -jar server.jar nogui"), None);
        // A parameter with a similar start and an empty value.
        let other = "javaw --gameDir C:\\mc --usernameFile x.txt --accessToken 0";
        assert_eq!(from_command_line(other), None);
        assert_eq!(from_command_line("javaw --gameDir C:\\mc --username"), None);
    }

    #[test]
    fn official_launcher_and_lunar_accounts() {
        // Minecraft Launcher: `username` holds an email, which must not become a nickname.
        let official = r#"{
            "accounts": {
                "aaa": {"minecraftProfile": {"id": "1", "name": "OldAccount"}, "username": "old@example.com"},
                "bbb": {"accessToken": "secret", "minecraftProfile": {"id": "2", "name": "GrayIsko1337"}, "username": "me@example.com"}
            },
            "activeAccountLocalId": "bbb",
            "mojangClientToken": "secret"
        }"#;
        assert_eq!(
            from_accounts_json(official).as_deref(),
            Some("GrayIsko1337")
        );

        // Lunar Client: the same shape, `username` is the nickname.
        let lunar = r#"{
            "accounts": {"ccc": {"accessToken": "secret", "minecraftProfile": {"id": "3", "name": "LunarNick"}, "username": "LunarNick", "type": "Xbox"}},
            "activeAccountLocalId": "ccc"
        }"#;
        assert_eq!(from_accounts_json(lunar).as_deref(), Some("LunarNick"));
    }

    #[test]
    fn prism_and_tlauncher_accounts() {
        // Prism Launcher: an array, the active entry is marked `active`.
        let prism = r#"{
            "accounts": [
                {"profile": {"id": "1", "name": "Second"}, "type": "MSA"},
                {"active": true, "profile": {"id": "2", "name": "PrismNick"}, "type": "MSA"}
            ],
            "formatVersion": 3
        }"#;
        assert_eq!(from_accounts_json(prism).as_deref(), Some("PrismNick"));

        // TLauncher: name in displayName/username, the selected entry is a separate field.
        let tlauncher = r#"{
            "accounts": {
                "u1": {"username": "First", "displayName": "First", "type": "free"},
                "u2": {"username": "Серый_Волк", "displayName": "Серый_Волк", "type": "free"}
            },
            "selectedAccountUUID": "u2"
        }"#;
        assert_eq!(from_accounts_json(tlauncher).as_deref(), Some("Серый_Волк"));
    }

    #[test]
    fn email_or_garbage_is_never_a_nickname() {
        // An account without a game profile: only the email remains, so there is no nickname.
        let only_email =
            r#"{"accounts": {"a": {"username": "me@example.com"}}, "activeAccountLocalId": "a"}"#;
        assert_eq!(from_accounts_json(only_email), None);
        assert_eq!(from_accounts_json("{}"), None);
        assert_eq!(from_accounts_json("не JSON"), None);
        assert_eq!(from_accounts_json(r#"{"accounts": 5}"#), None);
        assert_eq!(game_name("  Steve  ").as_deref(), Some("Steve"));
        assert_eq!(game_name("two words"), None);
        assert_eq!(game_name(&"x".repeat(MAX_CHARS + 1)), None);
        assert_eq!(game_name("a\u{1b}b"), None);
    }

    #[test]
    fn legacy_launcher_profiles() {
        let legacy = r#"{
            "authenticationDatabase": {
                "acc1": {"username": "a@example.com", "profiles": {"uuid1": {"displayName": "Other"}}},
                "acc2": {"username": "b@example.com", "profiles": {"uuid2": {"displayName": "LegacyNick"}}}
            },
            "selectedUser": {"account": "acc2", "profile": "uuid2"},
            "profiles": {}
        }"#;
        assert_eq!(
            from_legacy_profiles_json(legacy).as_deref(),
            Some("LegacyNick")
        );
        // Modern file: it has no accounts.
        let modern = r#"{"profiles": {"x": {"name": "Fabric", "type": "custom"}}, "settings": {}, "version": 3}"#;
        assert_eq!(from_legacy_profiles_json(modern), None);
    }

    #[test]
    fn nickname_from_game_log() {
        let log = "[12:00:01] [main/INFO]: Loading Minecraft 1.21.1\n\
                   [12:00:03] [Render thread/INFO]: Setting user: GrayIsko1337\n\
                   [12:00:09] [Render thread/INFO]: Backend library: LWJGL version 3.3.3\n";
        assert_eq!(from_log(log).as_deref(), Some("GrayIsko1337"));
        assert_eq!(from_log("[12:00:01] [main/INFO]: no user here\n"), None);
    }
}
