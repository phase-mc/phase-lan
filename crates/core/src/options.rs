//! Host and guest launch options (shared by the console and the program window).

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use phase_lang::tr;
use phase_minecraft::lan::AnnounceMode;
use phase_tunnel::{
    EndpointId, EndpointTicket, RelayUrl,
    access::Approver,
    endpoint::{self, NetworkMode, RelaySettings},
};

use crate::status::RelayInfo;

/// Environment variable with the relay password (if the password is not set explicitly).
pub const RELAY_TOKEN_ENV: &str = "PHASE_RELAY_TOKEN";
/// The former variable name (LanBridge): read if the new one is missing.
pub const LEGACY_RELAY_TOKEN_ENV: &str = "LANBRIDGE_RELAY_TOKEN";

/// Relay password from the environment: [`RELAY_TOKEN_ENV`], otherwise [`LEGACY_RELAY_TOKEN_ENV`].
/// An empty value counts as absent.
pub fn relay_token_from_env() -> Option<String> {
    [RELAY_TOKEN_ENV, LEGACY_RELAY_TOKEN_ENV]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|t| t.trim().to_string())
        .find(|t| !t.is_empty())
}
/// Default profile.
pub const DEFAULT_PROFILE: &str = "default";
/// How often to write ping by default.
pub const DEFAULT_STATUS_EVERY: Duration = Duration::from_secs(5);

/// The host's game port: a number or `auto` (find an open LAN world on this PC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamePort {
    Auto,
    /// The port was set manually: the room leads only to it.
    Fixed(u16),
    /// The program already found the world on this port itself (auto room): no need to search again,
    /// and from here on it works like `Auto`.
    Found(u16),
}

impl GamePort {
    /// The port was found by the program, not set by the host: if the world is opened again on a different
    /// port, the room moves there by itself.
    pub fn follows_world(self) -> bool {
        matches!(self, Self::Auto | Self::Found(_))
    }
}

impl std::str::FromStr for GamePort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        match s.parse::<u16>() {
            Ok(port) if port > 0 => Ok(Self::Fixed(port)),
            _ => Err(tr!(
                format!("«{s}»: нужен номер порта (1–65535) или auto"),
                format!("\"{s}\": expected a port number (1–65535) or auto")
            )),
        }
    }
}

/// Network and relay.
#[derive(Debug, Clone, Default)]
pub struct NetOptions {
    pub mode: NetworkMode,
    /// Own relays (empty means n0 relays, for checks only).
    pub relays: Vec<RelayUrl>,
    /// Password of your own relay. `None` means take it from the environment ([`relay_token_from_env`]).
    pub relay_token: Option<String>,
    /// Relay certificate file (PEM), if the relay has a self-signed certificate.
    pub relay_ca: Option<PathBuf>,
}

impl NetOptions {
    /// Relay settings. An error, for example, if the certificate file cannot be read.
    pub fn relay_settings(&self) -> Result<RelaySettings> {
        let token = match &self.relay_token {
            Some(token) => Some(token.trim().to_string()).filter(|t| !t.is_empty()),
            None => relay_token_from_env(),
        };
        let extra_roots = match &self.relay_ca {
            Some(path) => endpoint::read_ca_file(path)?,
            None => Vec::new(),
        };
        Ok(RelaySettings {
            urls: self.relays.clone(),
            token,
            extra_roots,
            peer_urls: Vec::new(),
        })
    }
}

/// Relay description for the interface.
pub(crate) fn relay_info(relay: &RelaySettings, network: NetworkMode) -> RelayInfo {
    if network == NetworkMode::LoopbackOnly {
        return RelayInfo {
            loopback_only: true,
            ..RelayInfo::default()
        };
    }
    let mut urls: Vec<String> = relay.urls.iter().map(|u| u.to_string()).collect();
    urls.extend(
        relay
            .peer_urls
            .iter()
            .map(|u| tr!(format!("{u} (relay хоста)"), format!("{u} (host's relay)"))),
    );
    RelayInfo {
        custom: relay.is_custom(),
        loopback_only: false,
        urls,
        needs_token: relay.token.is_some() && relay.is_custom(),
        needs_ca: !relay.extra_roots.is_empty(),
    }
}

/// Host voice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostVoice {
    /// Find the ports yourself (game log + game process ports) and add
    /// fallback ones: the game port and 24454. `game_dirs` are extra game folders to search
    /// for the log (if the launcher keeps the game in an unusual place).
    Auto { game_dirs: Vec<PathBuf> },
    /// Ports are set manually. The program does not check who holds them.
    Manual(Vec<u16>),
    /// Do not forward voice.
    Off,
}

impl Default for HostVoice {
    fn default() -> Self {
        Self::Auto {
            game_dirs: Vec::new(),
        }
    }
}

/// Which `mods` folder to use. The search order is in [`phase_minecraft::mods`]:
/// the running game, then the chosen folder, then the standard one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModsFolder {
    /// The folder the user chose.
    pub chosen: Option<PathBuf>,
    /// Look for a running game (`false` in autotests: the result must not depend on
    /// what is being played on this PC right now).
    pub detect_running_game: bool,
}

impl Default for ModsFolder {
    fn default() -> Self {
        Self {
            chosen: None,
            detect_running_game: true,
        }
    }
}

/// Mod distribution by the host. The set can be changed while the room is open
/// ([`crate::Session::set_host_mods`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostMods {
    /// Distribute mods to guests.
    pub share: bool,
    pub folder: ModsFolder,
    /// File names the host does not distribute (client-only mods: shaders, replay
    /// recording). The guest does not need them, but they do not count as extra for the guest either.
    pub excluded: Vec<String>,
}

/// Where the guest downloads mods from first: Modrinth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModrinthSource {
    /// API address (`https://api.modrinth.com`).
    pub api: String,
    /// Which addresses files may be downloaded from (the start of the address).
    pub cdn_prefixes: Vec<String>,
    /// How long to wait for the answer to "is there such a mod".
    pub lookup_timeout: Duration,
}

impl Default for ModrinthSource {
    fn default() -> Self {
        Self {
            api: "https://api.modrinth.com".into(),
            cdn_prefixes: vec!["https://cdn.modrinth.com/".into()],
            lookup_timeout: Duration::from_secs(3),
        }
    }
}

/// Mod sync for the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMods {
    pub folder: ModsFolder,
    /// Sync right after connecting, without waiting for the button.
    pub auto: bool,
    /// Look for the mod on Modrinth first (`None` means download only from the host; this is how it works
    /// in "this PC only" mode, where there is no internet by definition).
    pub modrinth: Option<ModrinthSource>,
}

impl Default for GuestMods {
    fn default() -> Self {
        Self {
            folder: ModsFolder::default(),
            auto: false,
            modrinth: Some(ModrinthSource::default()),
        }
    }
}

/// Profile folder: explicitly set or the standard one for the profile name.
pub(crate) fn resolve_profile_dir(profile: &str, explicit: &Option<PathBuf>) -> Result<PathBuf> {
    match explicit {
        Some(dir) => Ok(dir.clone()),
        None => phase_tunnel::identity::profile_dir(profile),
    }
}

/// Nickname for the session: set explicitly (after validation) or the profile's nickname. Reading the profile and
/// looking up the nickname in the game block, so they run in a separate thread.
pub(crate) async fn session_nickname(
    explicit: &Option<String>,
    dir: &std::path::Path,
    profile: &str,
) -> Result<String> {
    if let Some(nickname) = explicit {
        return crate::profiles::validate_nickname(nickname);
    }
    let (dir, profile) = (dir.to_path_buf(), profile.to_string());
    tokio::task::spawn_blocking(move || crate::profiles::resolve_nickname(&dir, &profile))
        .await
        .map_err(|err| {
            anyhow::anyhow!(
                "{}: {err}",
                tr!(
                    "определение ника аварийно завершилось",
                    "nickname detection crashed"
                )
            )
        })
}

/// Host parameters.
#[derive(Debug, Clone)]
pub struct HostOptions {
    pub profile: String,
    /// Profile folder instead of the standard `%LOCALAPPDATA%\Phase\profiles\<profile>`
    /// (for autotests).
    pub profile_dir: Option<PathBuf>,
    /// In-game nickname for this session. `None` means the profile's nickname (see [`crate::profiles`]).
    pub nickname: Option<String>,
    /// World/server port. `None` means game forwarding is off (only the channel check works).
    pub game_port: Option<GamePort>,
    /// World name for friends (by default, the name of the found world).
    pub motd: Option<String>,
    pub voice: HostVoice,
    /// Allow these devices in advance.
    pub allow: Vec<EndpointId>,
    /// Who answers the "let in?" question about a new device. `None` means new
    /// devices are not let in (only allowed ones).
    pub approver: Option<Arc<dyn Approver>>,
    pub net: NetOptions,
    pub status_every: Duration,
    /// Where to save the session report (`None` means do not save).
    pub reports_dir: Option<PathBuf>,
    /// Mod distribution to guests (off by default).
    pub mods: HostMods,
    /// Mod hash cache file (`None` means the cache is in memory only).
    pub mod_hash_cache: Option<PathBuf>,
    /// Which world to style the room in (datapack). By default the world opened by the game
    /// that listens on the room's port.
    pub world: crate::look::WorldSource,
}

impl HostOptions {
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            profile_dir: None,
            nickname: None,
            game_port: None,
            motd: None,
            voice: HostVoice::default(),
            allow: Vec::new(),
            approver: None,
            net: NetOptions::default(),
            status_every: DEFAULT_STATUS_EVERY,
            reports_dir: None,
            mods: HostMods::default(),
            mod_hash_cache: None,
            world: crate::look::WorldSource::default(),
        }
    }
}

/// Guest parameters.
#[derive(Debug, Clone)]
pub struct JoinOptions {
    pub profile: String,
    /// Profile folder instead of the standard one (for autotests).
    pub profile_dir: Option<PathBuf>,
    /// In-game nickname for this session. `None` means the profile's nickname (see [`crate::profiles`]).
    pub nickname: Option<String>,
    /// Ticket from the host.
    pub ticket: EndpointTicket,
    /// Local port for Minecraft (`None` means any free one).
    pub local_port: Option<u16>,
    /// How to show the world in the "Local network" list.
    pub announce: AnnounceMode,
    /// Forward voice (UDP).
    pub voice: bool,
    pub net: NetOptions,
    pub status_every: Duration,
    /// Where to save the session report (`None` means do not save).
    pub reports_dir: Option<PathBuf>,
    /// Mod sync with the host.
    pub mods: GuestMods,
    /// Mod hash cache file (`None` means the cache is in memory only).
    pub mod_hash_cache: Option<PathBuf>,
}

impl JoinOptions {
    pub fn new(profile: impl Into<String>, ticket: EndpointTicket) -> Self {
        Self {
            profile: profile.into(),
            profile_dir: None,
            nickname: None,
            ticket,
            local_port: None,
            announce: AnnounceMode::default(),
            voice: true,
            net: NetOptions::default(),
            status_every: DEFAULT_STATUS_EVERY,
            reports_dir: None,
            mods: GuestMods::default(),
            mod_hash_cache: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_port_parsing() {
        assert_eq!("auto".parse::<GamePort>(), Ok(GamePort::Auto));
        assert_eq!(" AUTO ".parse::<GamePort>(), Ok(GamePort::Auto));
        assert_eq!("25565".parse::<GamePort>(), Ok(GamePort::Fixed(25565)));
        assert!("0".parse::<GamePort>().is_err());
        assert!("x".parse::<GamePort>().is_err());
    }

    #[test]
    fn only_found_ports_follow_the_world() {
        assert!(GamePort::Auto.follows_world());
        assert!(GamePort::Found(51234).follows_world());
        assert!(!GamePort::Fixed(25565).follows_world());
    }
}
