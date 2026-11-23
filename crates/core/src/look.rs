//! Room styling in the game: header, tags, nickname colors, prefixes and highlight.
//!
//! The host's task. It collects who is in the room (events of the network core), under which
//! real nickname each one joined the world (the start of the game stream, see
//! [`phase_minecraft::handshake`]), and writes a hidden datapack into the host's world
//! ([`phase_minecraft::datapack`]). Guests need to install nothing.
//!
//! **Everything is free** (owner's decision of 2026-11-18): a custom header title, nickname colors,
//! prefixes, silhouette highlight, the footer in `Tab` and the number of seats in the room are available to any host.
//! Phase Pro rights ([`crate::entitlement`]) are not asked here.
//!
//! | | What is visible in the game |
//! |---|---|
//! | Header on join | `PHASE • code • online` or the host's own title |
//! | Host in `Tab` | the `[HOST]` tag, or their own color and prefix |
//! | Guest | no styling until the host sets a color and prefix for them |
//! | Silhouette highlight | the host for anyone, a guest for themselves if the host allowed it |
//!
//! The host is always in charge in their world: they can switch styling off entirely and forbid
//! guests to toggle the highlight. Roles here are labels only: Phase never grants
//! operator rights.
//!
//! **Seats.** The integrated server of a singleplayer game admits no more than eight players
//! ([`VANILLA_MAX_PLAYERS`]); Phase Companion lifts this limit, and then the room holds
//! as many as the host set ([`LookSettings::slots`], [`DEFAULT_SLOTS`] by default,
//! no more than [`MAX_SLOTS`]). See [`room_capacity`].
//!
//! **Life cycle.** While the room is open, the datapack in the active world is rewritten on
//! every change (with a short delay so as not to write on every click). When the room is
//! closed, the datapack becomes a cleanup: it removes the bossbar and `ph.*` teams from the world.
//! If the Phase Companion mod runs in the world, it applies everything (the `room.json` file), and
//! the datapack sleeps.
//!
//! **Room code.** There is no short-code service yet, so the header carries a short fingerprint of
//! the host ID in the same form (`K7F-2QX`): friends recognize the room by it, but it cannot be used
//! to connect yet, only the ticket can.

use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use phase_minecraft::{
    datapack::{self, PackMode, PackPlayer, PackRoom, PackState, Rank},
    handshake::is_valid_name,
    palette::{self, PhaseColor},
    world::{self, World},
};
use phase_tunnel::{fsutil, wire};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::session::Ctx;

/// File with styling settings in the host's profile folder.
pub const LOOK_FILE: &str = "look.json";
/// The host's tag in `Tab`.
pub const HOST_LABEL: &str = "HOST";
/// Maximum length of a prefix and of a custom header title.
pub const PREFIX_MAX_CHARS: usize = datapack::PREFIX_MAX_CHARS;
pub const TITLE_MAX_CHARS: usize = datapack::TITLE_MAX_CHARS;

/// The integrated server of a singleplayer game admits no more players than this (including the host).
pub const VANILLA_MAX_PLAYERS: u32 = 8;
/// Default seats in the room when Phase Companion runs.
pub const DEFAULT_SLOTS: u32 = 20;
/// Seats in the room: at least this many (the host and one guest).
pub const MIN_SLOTS: u32 = 2;
/// Seats in the room: at most this many: the integrated server runs on the host's PC in one thread.
/// Phase Companion has the same limit (`RoomPlan.maxPlayers`).
pub const MAX_SLOTS: u32 = 50;

/// How many players the room holds (including the host): without Phase Companion the game itself
/// admits no more than [`VANILLA_MAX_PLAYERS`], with it, as many as the host set.
pub fn room_capacity(slots: u32, companion: bool) -> u32 {
    let slots = slots.clamp(MIN_SLOTS, MAX_SLOTS);
    if companion {
        slots
    } else {
        slots.min(VANILLA_MAX_PLAYERS)
    }
}

/// How many styles are kept in the settings (old friends do not pile up forever).
const STYLES_KEEP: usize = 256;
/// Pause before writing: several changes in a row make one write.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// How often to check the world: it could have been opened, closed or changed.
const WORLD_RECHECK: Duration = Duration::from_secs(5);

// ------------------------------------------------------------------ settings

/// A player's style: a color from the Phase palette, a prefix and a silhouette highlight.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerStyle {
    /// Palette color identifier (`ice_sapphire`).
    pub color: Option<String>,
    /// Prefix without brackets, up to [`PREFIX_MAX_CHARS`] characters.
    pub prefix: Option<String>,
    /// Outline visible through walls.
    pub glow: bool,
}

impl PlayerStyle {
    fn is_empty(&self) -> bool {
        self.color.is_none() && self.prefix.is_none() && !self.glow
    }

    /// Validates a style typed by the user: the color only from the palette, the prefix
    /// by the game's rules. An empty prefix means "no prefix".
    pub fn validated(&self) -> Result<Self, String> {
        let color = match self
            .color
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            Some(id) => Some(
                palette::find(id)
                    .ok_or_else(|| {
                        phase_lang::tr!(
                            format!("цвета «{id}» нет в палитре Phase"),
                            format!("the color “{id}” is not in the Phase palette")
                        )
                    })?
                    .id
                    .to_string(),
            ),
            None => None,
        };
        let prefix = match self
            .prefix
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(prefix) => Some(datapack::validate_prefix(prefix).map_err(|err| {
                phase_lang::tr!(format!("префикс: {err}"), format!("prefix: {err}"))
            })?),
            None => None,
        };
        Ok(Self {
            color,
            prefix,
            glow: self.glow,
        })
    }

    /// The same for data from a file: unusable parts are silently dropped.
    fn lenient(&self) -> Self {
        Self {
            color: self
                .color
                .as_deref()
                .and_then(palette::find)
                .map(|color| color.id.to_string()),
            prefix: self
                .prefix
                .as_deref()
                .and_then(|prefix| datapack::validate_prefix(prefix).ok()),
            glow: self.glow,
        }
    }
}

/// The host's styling settings. Stored in the profile folder ([`LOOK_FILE`]); styles go by device
/// ID: a player can change the nickname, but not the ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LookSettings {
    /// Styling in the game is on.
    pub enabled: bool,
    /// A custom header title instead of `PHASE`.
    pub title: Option<String>,
    /// Guests toggle the highlight on themselves.
    pub guest_glow: bool,
    /// Styles the host set for players (and for themselves, by their own ID).
    pub styles: BTreeMap<String, PlayerStyle>,
    /// Verify players' accounts on joining the world (as usual in the game). `false` means let them in
    /// without verification: Phase Companion does this, and Phase ties each nickname to the
    /// device that joined with it first (`names.rs`). Free for everyone; does not
    /// depend on whether styling is on. `false` by default (letting in without
    /// verification is the owner's decision, for convenience), so nickname binding is mandatory.
    pub account_check: bool,
    /// Remove the "Powered by Phase" footer in `Tab`.
    pub hide_footer: bool,
    /// Seats in the room including the host, when Phase Companion runs
    /// ([`MIN_SLOTS`]–[`MAX_SLOTS`]). Without the mod the game admits no more than eight; see
    /// [`room_capacity`]. Does not depend on whether styling is on.
    pub slots: u32,
}

impl Default for LookSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            title: None,
            guest_glow: true,
            styles: BTreeMap::new(),
            account_check: false,
            hide_footer: false,
            slots: DEFAULT_SLOTS,
        }
    }
}

/// A string is a device ID (a public key: 64 hexadecimal digits).
fn is_device_id(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

impl LookSettings {
    /// Validates settings typed by the user.
    pub fn validated(&self) -> Result<Self, String> {
        let title = match self
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            Some(title) => Some(datapack::validate_title(title).map_err(|err| {
                phase_lang::tr!(format!("заголовок: {err}"), format!("title: {err}"))
            })?),
            None => None,
        };
        let mut styles = BTreeMap::new();
        for (device, style) in &self.styles {
            if !is_device_id(device) {
                return Err(phase_lang::tr!(
                    format!("«{device}» — не ID устройства"),
                    format!("“{device}” is not a device ID")
                ));
            }
            let style = style.validated()?;
            if !style.is_empty() {
                styles.insert(device.to_ascii_lowercase(), style);
            }
        }
        if styles.len() > STYLES_KEEP {
            return Err(phase_lang::tr!(
                format!("стилей не больше {STYLES_KEEP}"),
                format!("at most {STYLES_KEEP} styles")
            ));
        }
        if !(MIN_SLOTS..=MAX_SLOTS).contains(&self.slots) {
            return Err(phase_lang::tr!(
                format!("мест в комнате — от {MIN_SLOTS} до {MAX_SLOTS}"),
                format!("room slots: {MIN_SLOTS} to {MAX_SLOTS}")
            ));
        }
        Ok(Self {
            enabled: self.enabled,
            title,
            guest_glow: self.guest_glow,
            styles,
            account_check: self.account_check,
            hide_footer: self.hide_footer,
            slots: self.slots,
        })
    }

    /// The same for data from a file: unusable parts are dropped.
    fn lenient(&self) -> Self {
        Self {
            enabled: self.enabled,
            title: self
                .title
                .as_deref()
                .and_then(|title| datapack::validate_title(title).ok()),
            guest_glow: self.guest_glow,
            styles: self
                .styles
                .iter()
                .filter(|(device, _)| is_device_id(device))
                .map(|(device, style)| (device.to_ascii_lowercase(), style.lenient()))
                .filter(|(_, style)| !style.is_empty())
                .take(STYLES_KEEP)
                .collect(),
            account_check: self.account_check,
            hide_footer: self.hide_footer,
            slots: self.slots.clamp(MIN_SLOTS, MAX_SLOTS),
        }
    }

    fn set_style(&mut self, device_id: &str, style: PlayerStyle) {
        if style.is_empty() {
            self.styles.remove(device_id);
        } else {
            self.styles.insert(device_id.to_string(), style);
        }
    }
}

/// Styling settings of the profile. If the file is missing or damaged, the default settings.
pub fn load_settings(profile_dir: &Path) -> LookSettings {
    let file = profile_dir.join(LOOK_FILE);
    match fs::read_to_string(&file) {
        Ok(text) => match serde_json::from_str::<LookSettings>(text.trim_start_matches('\u{feff}'))
        {
            Ok(settings) => settings.lenient(),
            Err(err) => {
                tracing::warn!(
                    "{} повреждён ({err}) — оформление по умолчанию",
                    file.display()
                );
                LookSettings::default()
            }
        },
        Err(err) if err.kind() == ErrorKind::NotFound => LookSettings::default(),
        Err(err) => {
            tracing::warn!("не удалось прочитать {}: {err}", file.display());
            LookSettings::default()
        }
    }
}

/// Saves the styling settings to the profile folder.
pub fn save_settings(profile_dir: &Path, settings: &LookSettings) -> Result<()> {
    let file = profile_dir.join(LOOK_FILE);
    let json = serde_json::to_string_pretty(settings)?;
    fsutil::write_atomic(&file, format!("{json}\n").as_bytes()).with_context(|| {
        phase_lang::tr!(
            format!("не удалось сохранить оформление в {}", file.display()),
            format!("could not save the room style to {}", file.display())
        )
    })
}

/// Room code for the header in the game: six Crockford base32 characters from the start of the host ID
/// (`K7F-2QX`). The same host always gets the same code.
pub fn room_code(device_id: &str) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let head: String = device_id.chars().take(8).collect();
    let bits = u32::from_str_radix(&head, 16).unwrap_or(0) >> 2;
    let mut code = String::with_capacity(7);
    for index in 0..6 {
        if index == 3 {
            code.push('-');
        }
        let shift = 5 * (5 - index);
        code.push(ALPHABET[((bits >> shift) & 31) as usize] as char);
    }
    code
}

// ------------------------------------------------------------------ state for the window

/// What is going on with styling now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LookMode {
    /// The host turned styling off.
    Off,
    /// The active world was not found (game forwarding is off, the world is closed, the port is held by something that is not the game).
    NoWorld,
    /// Game version older than 1.16.
    Unsupported,
    /// The datapack is written, but the world was opened earlier: the game will load it on the next
    /// world open or on `/reload`.
    WaitingReload,
    /// The datapack works.
    Active,
    /// Phase Companion runs in the world: the mod applies the styling, at once.
    Companion,
}

/// A room player and how they look in the game.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LookPlayer {
    pub device_id: String,
    pub host: bool,
    /// The nickname the player gave in Phase.
    pub nickname: Option<String>,
    /// The nickname the player's game joined the world with (`None` means they have not joined the world yet).
    pub game_name: Option<String>,
    /// The in-game nickname fits game commands: without that styling is not applied.
    pub styled: bool,
    /// Prefix without brackets as it will be in the game (`HOST` or custom).
    pub label: Option<String>,
    /// Nickname color: a palette color identifier.
    pub color: Option<String>,
    pub glow: bool,
}

/// Room styling, for the host's window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LookStatus {
    pub mode: LookMode,
    /// Name of the active world's folder.
    pub world: Option<String>,
    /// Room code in the header.
    pub code: String,
    /// Header title as it is shown in the game.
    pub title: String,
    pub settings: LookSettings,
    /// The host and guests (in order of connection).
    pub players: Vec<LookPlayer>,
    /// Why the datapack is not written.
    pub error: Option<String>,
}

/// A host command to the styling task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookCommand {
    /// New settings as a whole (already validated).
    Settings(LookSettings),
    /// The style of one player.
    Style {
        device_id: String,
        style: PlayerStyle,
    },
}

/// Where to look for the host's world.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WorldSource {
    /// The world opened by the process that listens on the game port.
    #[default]
    Detect,
    /// This world folder (for autotests and your own server in an unusual place).
    Dir(PathBuf),
    /// Write the datapack nowhere.
    Off,
}

// ------------------------------------------------------------------ who looks how

/// A room participant.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    device_id: String,
    nickname: Option<String>,
    game_name: Option<String>,
    connections: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Resolved {
    label: Option<String>,
    color: Option<&'static PhaseColor>,
    glow: bool,
}

/// Changes the saved styling settings of the profile while there is no room (in an open
/// room [`LookCommand`] does the same).
pub fn update_stored(profile_dir: &Path, command: LookCommand) -> Result<LookSettings> {
    let mut settings = load_settings(profile_dir);
    apply_command(&mut settings, command);
    save_settings(profile_dir, &settings)?;
    Ok(settings)
}

fn apply_command(settings: &mut LookSettings, command: LookCommand) {
    match command {
        LookCommand::Settings(new) => *settings = new.lenient(),
        LookCommand::Style { device_id, style } if is_device_id(&device_id) => {
            settings.set_style(&device_id.to_ascii_lowercase(), style.lenient());
        }
        LookCommand::Style { .. } => {}
    }
}

/// How a player looks in the world: what the host set, and the host themselves with no prefix of their own gets
/// the `[HOST]` tag. A guest without a style from the host is not styled.
fn resolve(settings: &LookSettings, is_host: bool, member: &Member) -> Resolved {
    let assigned = settings.styles.get(&member.device_id);
    let prefix = assigned
        .and_then(|style| style.prefix.as_deref())
        .and_then(|prefix| datapack::validate_prefix(prefix).ok());
    Resolved {
        color: assigned
            .and_then(|style| style.color.as_deref())
            .and_then(palette::find),
        label: prefix.or_else(|| is_host.then(|| HOST_LABEL.to_string())),
        glow: assigned.is_some_and(|style| style.glow),
    }
}

/// A guest may toggle their own highlight: styling is on and the host allowed it.
fn can_glow(settings: &LookSettings) -> bool {
    settings.enabled && settings.guest_glow
}

// ------------------------------------------------------------------ writing to the world

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Writes the file only if the contents changed: the game rereads the datapack entirely, and
/// an extra write is extra disk work on every player join.
fn write_if_changed(path: &Path, content: &str) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    fsutil::write_atomic(path, content.as_bytes())
}

fn read_state(pack_dir: &Path) -> Option<PackState> {
    let text = fs::read_to_string(pack_dir.join(datapack::STATE_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes `<world>/phase/room.json` for Phase Companion: the room (`None` means no
/// styling) and the account check. The file is written when there is something to tell the mod (a room
/// or joining without verification) or when it already exists, so the mod learns that everything was removed.
fn write_companion_room(
    world_dir: &Path,
    room: Option<&PackRoom>,
    account_check: bool,
) -> Result<()> {
    let room_file = world_dir
        .join(datapack::COMPANION_FOLDER)
        .join(datapack::COMPANION_ROOM_FILE);
    if room.is_some() || !account_check || room_file.is_file() {
        write_if_changed(
            &room_file,
            &datapack::companion_room(room, account_check, phase_lang::current()),
        )
        .with_context(|| {
            phase_lang::tr!(
                format!("не удалось записать {}", room_file.display()),
                format!("could not write {}", room_file.display())
            )
        })?;
    }
    Ok(())
}

/// Writes the datapack into the world: `room` is the room (`None` means cleanup). `pack_active == false`
/// with an open room means Phase Companion applies the styling.
///
/// Into a world where there never was a Phase datapack, cleanup writes nothing. Returns
/// the datapack state (`None` means there is no datapack in the world).
///
/// `room.json` for the mod is written here too, with `account_check` (for cleanup it is `true`, joining
/// as in the game), even if there is no datapack in the world.
fn write_pack(
    world_dir: &Path,
    room: Option<&PackRoom>,
    pack_active: bool,
    protocol: Option<i32>,
    account_check: bool,
) -> Result<Option<PackState>> {
    let pack_dir = world_dir.join("datapacks").join(datapack::PACK_FOLDER);
    let old = read_state(&pack_dir);
    if room.is_none() && old.is_none() {
        write_companion_room(world_dir, None, account_check)?;
        return Ok(None);
    }
    let mode = match room {
        Some(room) if pack_active => PackMode::Active(room),
        _ => PackMode::Sleeping,
    };
    let known = old.as_ref().map(|state| state.teams.as_slice());
    let pack = datapack::generate(
        mode,
        protocol,
        known.unwrap_or_default(),
        phase_lang::current(),
    );
    for file in &pack.files {
        let path = file
            .path
            .split('/')
            .fold(pack_dir.clone(), |path, part| path.join(part));
        write_if_changed(&path, &file.content).with_context(|| {
            phase_lang::tr!(
                format!("не удалось записать {}", path.display()),
                format!("could not write {}", path.display())
            )
        })?;
    }
    // `tick` files of past versions: the game would look for a tag without a function 20 times a second.
    for stale in datapack::STALE_FILES {
        let path = stale
            .split('/')
            .fold(pack_dir.clone(), |path, part| path.join(part));
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    phase_lang::tr!(
                        format!("не удалось удалить {}", path.display()),
                        format!("could not delete {}", path.display())
                    )
                });
            }
        }
    }
    let active = matches!(mode, PackMode::Active(_));
    let now = unix_now();
    let state = match old {
        Some(old)
            if old.active == active
                && old.teams == pack.teams
                && old.schema == datapack::SCHEMA =>
        {
            old
        }
        old => {
            let state = PackState {
                schema: datapack::SCHEMA,
                active,
                teams: pack.teams,
                installed_unix: old
                    .map(|old| old.installed_unix)
                    .filter(|at| *at > 0)
                    .unwrap_or(now),
                updated_unix: now,
            };
            let json = serde_json::to_string_pretty(&state)?;
            fsutil::write_atomic(
                &pack_dir.join(datapack::STATE_FILE),
                format!("{json}\n").as_bytes(),
            )?;
            state
        }
    };
    write_companion_room(world_dir, room, account_check)?;
    Ok(Some(state))
}

// ------------------------------------------------------------------ leftovers after a crash

/// A file in the profile folder: worlds where Phase wrote an active datapack and has not removed it yet.
pub const WORLDS_FILE: &str = "look-worlds.json";

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct WorldList {
    worlds: Vec<PathBuf>,
}

fn read_worlds(profile_dir: &Path) -> Vec<PathBuf> {
    fs::read_to_string(profile_dir.join(WORLDS_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<WorldList>(&text).ok())
        .map(|list| list.worlds)
        .unwrap_or_default()
}

fn write_worlds(profile_dir: &Path, worlds: Vec<PathBuf>) {
    let file = profile_dir.join(WORLDS_FILE);
    if worlds.is_empty() {
        let _ = fs::remove_file(&file);
        return;
    }
    let result = serde_json::to_string_pretty(&WorldList { worlds })
        .map_err(anyhow::Error::from)
        .and_then(|json| fsutil::write_atomic(&file, format!("{json}\n").as_bytes()));
    if let Err(err) = result {
        tracing::warn!("не удалось сохранить {}: {err:#}", file.display());
    }
}

/// Remembers the world that holds an active datapack: if Phase crashes, at the next
/// start this list tells what to clean up.
fn remember_world(profile_dir: &Path, world_dir: &Path) {
    let mut worlds = read_worlds(profile_dir);
    if !worlds.iter().any(|known| known == world_dir) {
        worlds.push(world_dir.to_path_buf());
        write_worlds(profile_dir, worlds);
    }
}

fn forget_world(profile_dir: &Path, world_dir: &Path) {
    let mut worlds = read_worlds(profile_dir);
    let before = worlds.len();
    worlds.retain(|known| known != world_dir);
    if worlds.len() != before {
        write_worlds(profile_dir, worlds);
    }
}

/// Cleanup after Phase terminated abnormally: switches the datapack in every remembered world
/// to the "sleeping" form and deletes files of past versions ([`datapack::STALE_FILES`]). Called on
/// room start. Returns the worlds where the datapack was active.
///
/// **Why.** The Phase datapack stays in the world after Phase exits: the game runs `load` on
/// every world open. If Phase crashed, `apply` in the files still says "room open",
/// and the next world open would bring back the header, `ph.*` teams and an indefinite `glowing`
/// to the players of a long-closed room. A sleeping `apply` instead removes the bossbar, teams
/// and the highlight.
///
/// **What this does not do.** A running world does not reread files, and Phase does not call
/// `reload` itself: for an already connected player `glowing` (999999 ticks), teams and the header
/// stay until the next world open or the host's `/trigger phase.sync`. Without a running
/// game the effect cannot be removed from the player at all (it lies in the player data inside the world).
pub fn cleanup_leftovers(profile_dir: &Path, protocol: Option<i32>) -> Vec<PathBuf> {
    let mut cleaned = Vec::new();
    let mut keep = Vec::new();
    for dir in read_worlds(profile_dir) {
        let was_active = read_state(&dir.join("datapacks").join(datapack::PACK_FOLDER))
            .is_some_and(|state| state.active);
        // `write_pack(None)` in a world without our datapack writes nothing.
        match write_pack(&dir, None, false, protocol, true) {
            Ok(_) => {
                if was_active {
                    cleaned.push(dir);
                }
            }
            Err(err) => {
                tracing::warn!("уборка в {}: {err:#}", dir.display());
                keep.push(dir);
            }
        }
    }
    write_worlds(profile_dir, keep);
    cleaned
}

// ------------------------------------------------------------------ task

/// What the styling task needs to know about the room at start.
#[derive(Debug, Clone)]
pub(crate) struct LookSetup {
    pub profile_dir: PathBuf,
    pub host_device: String,
    /// The host's nickname in Phase (the in-game nickname is refined from the running game).
    pub host_nickname: String,
    pub game_port: Option<u16>,
    /// LAN world name: helps pick the world if several are open.
    pub motd: Option<String>,
    /// The host's game protocol number (from the status query).
    pub protocol: Option<i32>,
    pub world: WorldSource,
}

#[derive(Debug)]
struct Inner {
    setup: LookSetup,
    settings: LookSettings,
    /// The settings changed and are not saved to the file yet.
    unsaved: bool,
    guests: Vec<Member>,
    /// The nickname the host's game runs under.
    host_game_name: Option<String>,
    /// Seats in the room now ([`room_capacity`]).
    max_players: u32,
    companion: bool,
    world: Option<String>,
    /// The room's world folder: the last found world. It stays even after the host
    /// left the world; it shows that the world was opened again (see `follow.rs`).
    world_dir: Option<PathBuf>,
    /// The world the room's active datapack is written to.
    written: Option<PathBuf>,
    mode: LookMode,
    error: Option<String>,
    closed: bool,
    published: Option<LookStatus>,
}

/// The host's room styling. Cheap to clone: all copies are one state.
#[derive(Clone)]
pub(crate) struct Look {
    inner: Arc<Mutex<Inner>>,
    dirty: Arc<Notify>,
    /// Writes to the world go one at a time: the task and the room closing do not write at once.
    writing: Arc<tokio::sync::Mutex<()>>,
    ctx: Ctx,
}

impl std::fmt::Debug for Look {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Look").finish_non_exhaustive()
    }
}

/// What the blocking part needs to write the datapack.
struct Job {
    setup: LookSetup,
    settings: LookSettings,
    save_settings: bool,
    guests: Vec<Member>,
    host_game_name: Option<String>,
    max_players: u32,
    written: Option<PathBuf>,
    closed: bool,
}

struct Outcome {
    world: Option<String>,
    /// Found world folder.
    world_dir: Option<PathBuf>,
    written: Option<PathBuf>,
    host_game_name: Option<String>,
    mode: LookMode,
    companion: bool,
    error: Option<String>,
}

impl Member {
    fn host(setup: &LookSetup, game_name: Option<&str>) -> Self {
        Self {
            device_id: setup.host_device.clone(),
            nickname: Some(setup.host_nickname.clone()),
            game_name: game_name
                .map(str::to_string)
                .or_else(|| Some(setup.host_nickname.clone()).filter(|name| is_valid_name(name))),
            connections: 1,
        }
    }
}

/// The room as the game will show it.
fn pack_room(job: &Job) -> PackRoom {
    let setup = &job.setup;
    let host = Member::host(setup, job.host_game_name.as_deref());
    let members = std::iter::once((true, &host)).chain(job.guests.iter().map(|g| (false, g)));
    let players = members
        .filter_map(|(is_host, member)| {
            let name = member.game_name.clone()?;
            let look = resolve(&job.settings, is_host, member);
            Some(PackPlayer {
                name,
                rank: if is_host { Rank::Host } else { Rank::Guest },
                label: look.label,
                color: look.color,
                glow: look.glow,
            })
        })
        .collect();
    PackRoom {
        title: job
            .settings
            .title
            .clone()
            .unwrap_or_else(|| datapack::DEFAULT_TITLE.to_string()),
        code: room_code(&setup.host_device),
        max_players: job.max_players,
        footer: !job.settings.hide_footer,
        host_name: host.game_name,
        players,
    }
}

/// The blocking part: find the world and write the datapack.
fn apply(job: Job) -> Outcome {
    let setup = &job.setup;
    if job.save_settings
        && let Err(err) = save_settings(&setup.profile_dir, &job.settings)
    {
        tracing::warn!("{err:#}");
    }
    let mut outcome = Outcome {
        world: None,
        world_dir: None,
        written: job.written.clone(),
        host_game_name: job.host_game_name.clone(),
        mode: LookMode::Off,
        companion: false,
        error: None,
    };
    // The world is needed for styling and for joining without an account check (the mod does it).
    let wanted = (job.settings.enabled || !job.settings.account_check) && !job.closed;
    let found = if !wanted || !datapack::supports(setup.protocol) {
        None
    } else {
        match &setup.world {
            WorldSource::Off => None,
            WorldSource::Dir(dir) => World::at(dir).filter(|world| world.open),
            WorldSource::Detect => setup
                .game_port
                .and_then(|port| world::find_active(port, setup.motd.as_deref()))
                .map(|active| {
                    if let Some(name) = active.username.filter(|name| is_valid_name(name)) {
                        outcome.host_game_name = Some(name);
                    }
                    active.world
                }),
        }
    };
    // The room moved to another world (or closed, or styling is off): in the previous one, cleanup.
    // If the world was simply closed, the datapack stays operational: the host often re-enters the same world,
    // and the game reads the files exactly at the moment of opening. Cleanup here could be written before
    // the re-entry, and the world would open without styling, while a sleeping datapack
    // does not open `/trigger phase.sync`. Leftovers are removed by `close` or `cleanup_leftovers`.
    let moved = !wanted || found.is_some();
    if let Some(old) = &job.written
        && moved
        && found.as_ref().is_none_or(|world| world.dir != *old)
    {
        match write_pack(old, None, false, setup.protocol, true) {
            Ok(_) => {
                outcome.written = None;
                forget_world(&setup.profile_dir, old);
            }
            Err(err) => outcome.error = Some(format!("{err:#}")),
        }
    }
    outcome.mode = if !job.settings.enabled || job.closed {
        LookMode::Off
    } else if !datapack::supports(setup.protocol) {
        LookMode::Unsupported
    } else {
        LookMode::NoWorld
    };
    let Some(world) = found else {
        return outcome;
    };
    outcome.world = Some(world.name.clone());
    outcome.world_dir = Some(world.dir.clone());
    outcome.companion = world.companion_alive();
    // First the write to the list, then to the world: a crash in between leaves no unknown files.
    remember_world(&setup.profile_dir, &world.dir);
    let job = Job {
        host_game_name: outcome.host_game_name.clone(),
        ..job
    };
    let room = pack_room(&job);
    let account_check = job.settings.account_check;
    if !job.settings.enabled {
        // Styling is off but joining without verification is not: only room.json is in the world.
        match write_pack(&world.dir, None, false, job.setup.protocol, account_check) {
            Ok(_) => outcome.written = Some(world.dir.clone()),
            Err(err) => outcome.error = Some(format!("{err:#}")),
        }
        return outcome;
    }
    match write_pack(
        &world.dir,
        Some(&room),
        !outcome.companion,
        job.setup.protocol,
        account_check,
    ) {
        Ok(state) => {
            outcome.written = Some(world.dir.clone());
            // The datapack appeared in the world after it was opened: the game does not know about it.
            let loaded = match (state, world.opened) {
                (Some(state), Some(opened)) => {
                    UNIX_EPOCH + Duration::from_secs(state.installed_unix) <= opened
                }
                _ => false,
            };
            outcome.mode = if outcome.companion {
                LookMode::Companion
            } else if loaded {
                LookMode::Active
            } else {
                LookMode::WaitingReload
            };
        }
        Err(err) => outcome.error = Some(format!("{err:#}")),
    }
    outcome
}

impl Look {
    pub fn new(setup: LookSetup, ctx: Ctx) -> Self {
        let settings = load_settings(&setup.profile_dir);
        Self {
            inner: Arc::new(Mutex::new(Inner {
                // The mod is not found in the world yet: for now the seats are as in the game itself.
                max_players: room_capacity(settings.slots, false),
                setup,
                settings,
                unsaved: false,
                guests: Vec::new(),
                host_game_name: None,
                companion: false,
                world: None,
                world_dir: None,
                written: None,
                mode: LookMode::NoWorld,
                error: None,
                closed: false,
                published: None,
            })),
            dirty: Arc::new(Notify::new()),
            writing: Arc::new(tokio::sync::Mutex::new(())),
            ctx,
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().expect("look state lock")
    }

    /// Changes the state, shows it to the window at once and queues the datapack write.
    fn change(&self, change: impl FnOnce(&mut Inner)) {
        change(&mut self.inner());
        self.publish();
        self.dirty.notify_one();
    }

    /// The room's world folder (the last found world; `None` means no world was found yet).
    pub fn world_dir(&self) -> Option<PathBuf> {
        self.inner().world_dir.clone()
    }

    /// The host opened the world again, and the game took a different port: the world is searched by the new port
    /// (`motd` is its name, helps pick the world if several are open).
    pub fn set_game_port(&self, port: u16, motd: Option<String>) {
        self.change(|inner| {
            inner.setup.game_port = Some(port);
            if motd.is_some() {
                inner.setup.motd = motd;
            }
        });
    }

    /// How many players the room holds now (including the host).
    pub fn max_players(&self) -> u32 {
        self.inner().max_players
    }

    /// For joining the world: whether players' accounts are verified and the host's game nickname (it is always
    /// tied to the host).
    pub fn admission(&self) -> (bool, Option<String>) {
        let inner = self.inner();
        let host = Member::host(&inner.setup, inner.host_game_name.as_deref());
        (inner.settings.account_check, host.game_name)
    }

    /// State for the window.
    pub fn status(&self) -> LookStatus {
        let inner = self.inner();
        let setup = &inner.setup;
        let host = Member::host(setup, inner.host_game_name.as_deref());
        let members = std::iter::once((true, &host)).chain(inner.guests.iter().map(|g| (false, g)));
        let players = members
            .map(|(is_host, member)| {
                let look = resolve(&inner.settings, is_host, member);
                LookPlayer {
                    device_id: member.device_id.clone(),
                    host: is_host,
                    nickname: member.nickname.clone(),
                    styled: member.game_name.as_deref().is_some_and(is_valid_name),
                    game_name: member.game_name.clone(),
                    label: look.label,
                    color: look.color.map(|color| color.id.to_string()),
                    glow: look.glow,
                }
            })
            .collect();
        LookStatus {
            mode: if inner.settings.enabled {
                inner.mode
            } else {
                LookMode::Off
            },
            world: inner.world.clone(),
            code: room_code(&setup.host_device),
            title: inner
                .settings
                .title
                .clone()
                .unwrap_or_else(|| datapack::DEFAULT_TITLE.to_string()),
            settings: inner.settings.clone(),
            players,
            error: inner.error.clone(),
        }
    }

    /// Shows the state to the window if it changed.
    fn publish(&self) {
        let status = self.status();
        {
            let mut inner = self.inner();
            if inner.published.as_ref() == Some(&status) {
                return;
            }
            inner.published = Some(status.clone());
        }
        self.ctx.update(|s| {
            if let Some(host) = &mut s.host {
                host.look = status;
            }
        });
    }

    // ---- room events

    pub fn guest_connected(&self, device_id: &str, nickname: Option<&str>) {
        self.change(
            |inner| match inner.guests.iter_mut().find(|g| g.device_id == device_id) {
                Some(guest) => {
                    guest.connections += 1;
                    if nickname.is_some() {
                        guest.nickname = nickname.map(str::to_string);
                    }
                }
                None => inner.guests.push(Member {
                    device_id: device_id.to_string(),
                    nickname: nickname.map(str::to_string),
                    game_name: None,
                    connections: 1,
                }),
            },
        );
    }

    pub fn guest_renamed(&self, device_id: &str, nickname: &str) {
        self.change(|inner| {
            if let Some(guest) = inner.guests.iter_mut().find(|g| g.device_id == device_id) {
                guest.nickname = Some(nickname.to_string());
            }
        });
    }

    /// The guest's game joins the world under this nickname (from the start of the game stream).
    pub fn guest_game_name(&self, device_id: &str, name: &str) {
        self.change(|inner| {
            if let Some(guest) = inner.guests.iter_mut().find(|g| g.device_id == device_id) {
                guest.game_name = Some(name.to_string());
            }
        });
    }

    pub fn guest_disconnected(&self, device_id: &str) {
        self.change(|inner| {
            if let Some(at) = inner.guests.iter().position(|g| g.device_id == device_id) {
                if inner.guests[at].connections > 1 {
                    inner.guests[at].connections -= 1;
                } else {
                    inner.guests.remove(at);
                }
            }
        });
    }

    // ---- host and guest commands

    /// A host command: settings or a player's style. The number of seats changes on the next
    /// write (see [`Look::run`]).
    pub fn command(&self, command: LookCommand) {
        self.change(|inner| {
            apply_command(&mut inner.settings, command);
            inner.unsaved = true;
        });
    }

    /// A guest asks about their look in the world and may toggle the highlight on themselves.
    /// Called in a network core task: memory only, the write goes in the background.
    pub fn guest_request(&self, device_id: &str, request: wire::LookRequest) -> wire::LookState {
        let mut changed = false;
        let state = {
            let mut inner = self.inner();
            let Some(guest) = inner
                .guests
                .iter()
                .find(|g| g.device_id == device_id)
                .cloned()
            else {
                return wire::LookState::default();
            };
            let allowed = can_glow(&inner.settings);
            if let Some(glow) = request.glow
                && allowed
            {
                let mut style = inner
                    .settings
                    .styles
                    .get(device_id)
                    .cloned()
                    .unwrap_or_default();
                if style.glow != glow {
                    style.glow = glow;
                    inner.settings.set_style(device_id, style);
                    inner.unsaved = true;
                    changed = true;
                }
            }
            let look = resolve(&inner.settings, false, &guest);
            wire::LookState {
                enabled: inner.settings.enabled,
                game_name: guest.game_name,
                color: look.color.map(|color| color.id.to_string()),
                prefix: look.label,
                glow: look.glow,
                can_glow: allowed,
            }
        };
        if changed {
            self.publish();
            self.dirty.notify_one();
        }
        state
    }

    // ---- writing

    async fn refresh(&self, on_capacity: &(dyn Fn(bool, u32) + Send + Sync)) {
        let _writing = self.writing.lock().await;
        let job = {
            let mut inner = self.inner();
            Job {
                setup: inner.setup.clone(),
                settings: inner.settings.clone(),
                save_settings: std::mem::take(&mut inner.unsaved),
                guests: inner.guests.clone(),
                host_game_name: inner.host_game_name.clone(),
                max_players: inner.max_players,
                written: inner.written.clone(),
                closed: inner.closed,
            }
        };
        // Seats follow this write's settings (they are already saved): a change that arrives during
        // the write will be applied by the next one.
        let slots = job.settings.slots;
        let outcome = match tokio::task::spawn_blocking(move || apply(job)).await {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!("запись оформления аварийно завершилась: {err}");
                return;
            }
        };
        let (capacity_changed, first_error) = {
            let mut inner = self.inner();
            let first_error = outcome.error.clone().filter(|_| inner.error.is_none());
            // Seats depend on the mod in the world and on the host's "Seats in the room" setting.
            let max_players = room_capacity(slots, outcome.companion);
            let changed = inner.companion != outcome.companion || inner.max_players != max_players;
            inner.companion = outcome.companion;
            inner.max_players = max_players;
            inner.world = outcome.world;
            if outcome.world_dir.is_some() {
                inner.world_dir = outcome.world_dir;
            }
            inner.written = outcome.written;
            inner.host_game_name = outcome.host_game_name;
            inner.mode = outcome.mode;
            inner.error = outcome.error;
            (
                changed.then_some((outcome.companion, max_players)),
                first_error,
            )
        };
        if let Some(error) = first_error {
            self.ctx.warn(phase_lang::tr!(
                format!("Оформление в игре не записано: {error}"),
                format!("Room style could not be written to the game: {error}")
            ));
        }
        if let Some((companion, max_players)) = capacity_changed {
            on_capacity(companion, max_players);
            // The header and `room.json` get the new number of seats.
            self.dirty.notify_one();
        }
        self.publish();
    }

    /// Works while the room is open: writes the datapack on every change and watches
    /// the world. `on_capacity(companion, max_players)` is called when Phase Companion appeared or
    /// disappeared in the world or the host changed the number of seats.
    pub async fn run(self, on_capacity: impl Fn(bool, u32) + Send + Sync + 'static) {
        // Leftovers of a previous session that did not manage to clean up after itself (see `cleanup_leftovers`).
        {
            let (dir, protocol) = {
                let inner = self.inner();
                (inner.setup.profile_dir.clone(), inner.setup.protocol)
            };
            let _writing = self.writing.lock().await;
            match tokio::task::spawn_blocking(move || cleanup_leftovers(&dir, protocol)).await {
                Ok(cleaned) if !cleaned.is_empty() => {
                    tracing::info!("убраны остатки прошлой сессии в {} мирах", cleaned.len());
                }
                Ok(_) => {}
                Err(err) => tracing::warn!("уборка остатков аварийно завершилась: {err}"),
            }
        }
        loop {
            self.refresh(&on_capacity).await;
            tokio::select! {
                _ = self.dirty.notified() => tokio::time::sleep(DEBOUNCE).await,
                _ = tokio::time::sleep(WORLD_RECHECK) => {}
            }
        }
    }

    /// The room is closing: the datapack in the world becomes a cleanup.
    pub async fn close(&self) {
        self.inner().closed = true;
        self.refresh(&|_, _| {}).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const GUEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phase-look-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn member(device_id: &str) -> Member {
        Member {
            device_id: device_id.into(),
            nickname: None,
            game_name: Some("Steve".into()),
            connections: 1,
        }
    }

    fn style(color: Option<&str>, prefix: Option<&str>, glow: bool) -> PlayerStyle {
        PlayerStyle {
            color: color.map(str::to_string),
            prefix: prefix.map(str::to_string),
            glow,
        }
    }

    fn settings_with(device_id: &str, style: PlayerStyle) -> LookSettings {
        let mut settings = LookSettings::default();
        settings.set_style(device_id, style);
        settings
    }

    fn look_of(resolved: &Resolved) -> (Option<&str>, Option<&str>, bool) {
        (
            resolved.label.as_deref(),
            resolved.color.map(|color| color.id),
            resolved.glow,
        )
    }

    #[test]
    fn any_host_styles_everyone_and_has_the_host_tag_by_default() {
        // Without settings: the host has the HOST tag, a guest is not styled.
        let plain = LookSettings::default();
        let host = resolve(&plain, true, &member(HOST));
        assert_eq!(look_of(&host), (Some("HOST"), None, false));
        assert_eq!(resolve(&plain, false, &member(GUEST)), Resolved::default());

        // The host set a style for themselves and the guest: Phase Pro is not needed for that.
        let mut settings = settings_with(HOST, style(Some("ice_sapphire"), Some("KING"), true));
        settings.set_style(GUEST, style(Some("emerald_mint"), Some("Билдер"), true));
        assert_eq!(
            look_of(&resolve(&settings, true, &member(HOST))),
            (Some("KING"), Some("ice_sapphire"), true)
        );
        assert_eq!(
            look_of(&resolve(&settings, false, &member(GUEST))),
            (Some("Билдер"), Some("emerald_mint"), true)
        );

        // The host set only a color for themselves: the HOST tag stays.
        let only_color = settings_with(HOST, style(Some("rose_quartz"), None, false));
        assert_eq!(
            look_of(&resolve(&only_color, true, &member(HOST))),
            (Some("HOST"), Some("rose_quartz"), false)
        );
        // An unusable prefix from the file does not get into the game.
        let odd = settings_with(GUEST, style(None, Some("a\"b"), false));
        assert_eq!(resolve(&odd, false, &member(GUEST)).label, None);
    }

    #[test]
    fn guests_may_toggle_glow_when_the_host_allows_it() {
        let mut settings = LookSettings::default();
        assert!(can_glow(&settings), "allowed by default, free");
        settings.guest_glow = false;
        assert!(!can_glow(&settings), "the host forbade it");
        settings.guest_glow = true;
        settings.enabled = false;
        assert!(!can_glow(&settings), "styling is off");
    }

    #[test]
    fn room_capacity_follows_the_game_limit_and_the_host_setting() {
        assert_eq!(LookSettings::default().slots, DEFAULT_SLOTS);
        assert_eq!(DEFAULT_SLOTS, 20);
        // Without the mod the game itself admits no more than eight; fewer if the host decided so.
        assert_eq!(room_capacity(20, false), VANILLA_MAX_PLAYERS);
        assert_eq!(room_capacity(50, false), 8);
        assert_eq!(room_capacity(4, false), 4);
        // With Phase Companion: as many as the host set, within 2–50.
        assert_eq!(room_capacity(20, true), 20);
        assert_eq!(room_capacity(50, true), MAX_SLOTS);
        assert_eq!(room_capacity(4, true), 4);
        assert_eq!(room_capacity(500, true), 50);
        assert_eq!(room_capacity(0, true), MIN_SLOTS);
    }

    #[test]
    fn slots_are_checked_from_the_window_and_clamped_from_the_file() {
        for good in [MIN_SLOTS, 8, 20, 35, MAX_SLOTS] {
            let settings = LookSettings {
                slots: good,
                ..LookSettings::default()
            };
            assert_eq!(settings.validated().unwrap().slots, good);
        }
        for bad in [0, 1, 51, 1000] {
            let settings = LookSettings {
                slots: bad,
                ..LookSettings::default()
            };
            let err = settings.validated().unwrap_err();
            assert!(err.contains("от 2 до 50"), "{bad}: {err}");
        }

        let dir = temp_dir("slots");
        // An old look.json without the field: 20 seats.
        fs::write(dir.join(LOOK_FILE), r#"{"enabled":true}"#).unwrap();
        assert_eq!(load_settings(&dir).slots, 20);
        // Edited by hand: clamped into the limits.
        fs::write(dir.join(LOOK_FILE), r#"{"slots":999}"#).unwrap();
        assert_eq!(load_settings(&dir).slots, MAX_SLOTS);
        fs::write(dir.join(LOOK_FILE), r#"{"slots":1}"#).unwrap();
        assert_eq!(load_settings(&dir).slots, MIN_SLOTS);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stored_settings_change_without_a_room_for_any_host() {
        let dir = temp_dir("stored");
        let wanted = LookSettings {
            enabled: false,
            title: Some("Vanilla Crew".into()),
            guest_glow: false,
            styles: BTreeMap::from([(GUEST.to_string(), style(Some("deep_ocean"), None, true))]),
            account_check: true,
            hide_footer: true,
            slots: 32,
        };
        // Everything is free: title, styles, footer and seats can be changed by any host.
        let saved = update_stored(&dir, LookCommand::Settings(wanted.clone())).unwrap();
        assert_eq!(saved, wanted);
        assert_eq!(load_settings(&dir), wanted);
        assert!(
            !LookSettings::default().account_check,
            "by default players are let in without verification (owner's decision)"
        );
        // An old look.json without the field: as by default.
        assert!(
            !serde_json::from_str::<LookSettings>(r#"{"enabled":true}"#)
                .unwrap()
                .account_check
        );

        let styled = update_stored(
            &dir,
            LookCommand::Style {
                device_id: GUEST.into(),
                style: style(Some("deep_ocean"), Some("VIP"), false),
            },
        )
        .unwrap();
        assert_eq!(
            styled.styles[GUEST],
            style(Some("deep_ocean"), Some("VIP"), false)
        );
        // An empty style removes the entry; someone else's key is not accepted.
        let cleared = LookCommand::Style {
            device_id: GUEST.into(),
            style: PlayerStyle::default(),
        };
        assert!(update_stored(&dir, cleared).unwrap().styles.is_empty());
        let junk = LookCommand::Style {
            device_id: "junk".into(),
            style: style(None, Some("X"), false),
        };
        assert!(update_stored(&dir, junk).unwrap().styles.is_empty());
        assert_eq!(load_settings(&dir).title.as_deref(), Some("Vanilla Crew"));
        assert_eq!(load_settings(&dir).slots, 32);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn settings_are_validated_and_survive_save_and_load() {
        let dir = temp_dir("settings");
        assert_eq!(load_settings(&dir), LookSettings::default());
        assert!(LookSettings::default().enabled && LookSettings::default().guest_glow);

        let mut settings = LookSettings {
            title: Some("  Vanilla   Crew ".into()),
            ..LookSettings::default()
        };
        settings.styles.insert(
            GUEST.to_uppercase(),
            style(Some("ice_sapphire"), Some(" VIP "), true),
        );
        // An empty style is not stored.
        settings
            .styles
            .insert(HOST.into(), style(None, Some("  "), false));
        let clean = settings.validated().unwrap();
        assert_eq!(clean.title.as_deref(), Some("Vanilla Crew"));
        assert_eq!(
            clean.styles,
            BTreeMap::from([(
                GUEST.to_string(),
                style(Some("ice_sapphire"), Some("VIP"), true)
            )])
        );
        save_settings(&dir, &clean).unwrap();
        assert_eq!(load_settings(&dir), clean);

        // Errors are named in words: the window shows them as is.
        for (bad, part) in [
            (style(Some("hot_pink"), None, false), "палитре"),
            (style(None, Some("123456789"), false), "8"),
            (style(None, Some("a\"b"), false), "префикс"),
        ] {
            let err = settings_with(GUEST, bad).validated().unwrap_err();
            assert!(err.contains(part), "{err}");
        }
        assert!(
            settings_with("not-an-id", style(None, Some("X"), false))
                .validated()
                .is_err()
        );
        let long_title = LookSettings {
            title: Some("x".repeat(33)),
            ..LookSettings::default()
        };
        assert!(long_title.validated().unwrap_err().contains("заголовок"));

        // The file was edited by hand: unusable parts are dropped, the rest is read.
        fs::write(
            dir.join(LOOK_FILE),
            format!(
                r#"{{"enabled":false,"title":"a\"b","styles":{{"{GUEST}":{{"color":"nope","prefix":"OK","glow":true}},"junk":{{"prefix":"X"}}}},"future_field":1}}"#
            ),
        )
        .unwrap();
        let loaded = load_settings(&dir);
        assert!(!loaded.enabled && loaded.guest_glow);
        assert_eq!(loaded.title, None);
        assert_eq!(
            loaded.styles,
            BTreeMap::from([(GUEST.to_string(), style(None, Some("OK"), true))])
        );
        fs::write(dir.join(LOOK_FILE), "не JSON").unwrap();
        assert_eq!(load_settings(&dir), LookSettings::default());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn room_code_is_short_stable_and_well_formed() {
        let code = room_code(HOST);
        assert_eq!(code, room_code(HOST));
        assert_ne!(code, room_code(GUEST));
        for id in [
            HOST,
            GUEST,
            "00000000",
            "ffffffff",
            "1603817a9c",
            "",
            "не ID",
        ] {
            let code = room_code(id);
            let bytes = code.as_bytes();
            assert_eq!(bytes.len(), 7, "{code}");
            assert_eq!(bytes[3], b'-');
            assert!(
                code.chars()
                    .filter(|c| *c != '-')
                    .all(|c| "0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(c)),
                "{code}"
            );
            assert!(datapack::is_valid_code(&code));
        }
        assert_eq!(room_code("ffffffff"), "ZZZ-ZZZ");
        assert_eq!(room_code("00000000"), "000-000");
    }

    #[test]
    fn pack_is_installed_updated_and_put_to_sleep() {
        let world = temp_dir("pack");
        let pack = world.join("datapacks").join(datapack::PACK_FOLDER);
        let apply = pack
            .join("data")
            .join("phase")
            .join("function")
            .join("apply.mcfunction");
        let room_json = world.join("phase").join("room.json");

        // Into a world where there was no Phase, cleanup writes nothing.
        assert_eq!(
            write_pack(&world, None, false, Some(767), true).unwrap(),
            None
        );
        assert!(!pack.exists() && !room_json.exists());

        let room = PackRoom {
            title: "PHASE".into(),
            code: "K7F-2QX".into(),
            max_players: 5,
            footer: true,
            host_name: Some("Steve".into()),
            players: vec![PackPlayer {
                name: "Steve".into(),
                rank: Rank::Host,
                label: Some("HOST".into()),
                color: None,
                glow: false,
            }],
        };
        // `tick` files of a past datapack version lie in the world.
        let stale_paths: Vec<_> = datapack::STALE_FILES
            .iter()
            .map(|file| {
                file.split('/')
                    .fold(pack.clone(), |path, part| path.join(part))
            })
            .collect();
        for path in &stale_paths {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "old\n").unwrap();
        }
        let installed = write_pack(&world, Some(&room), true, Some(767), true)
            .unwrap()
            .unwrap();
        assert!(installed.active && installed.installed_unix > 0);
        assert_eq!(installed.teams, ["ph.a00"]);
        assert!(pack.join("pack.mcmeta").is_file());
        assert!(
            pack.join("data")
                .join("phase")
                .join("functions")
                .join("watch.mcfunction")
                .is_file()
        );
        assert!(
            stale_paths.iter().all(|path| !path.exists()),
            "tick files of past versions deleted"
        );
        assert!(fs::read_to_string(&apply).unwrap().contains("[HOST] "));
        assert!(
            fs::read_to_string(&room_json)
                .unwrap()
                .contains("\"active\": true")
        );
        // No temporary files are left.
        assert!(
            fs::read_dir(&pack)
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().contains(".tmp"))
        );

        // Writing the same thing again does not touch the files.
        let before = fs::metadata(&apply).unwrap().modified().unwrap();
        let again = write_pack(&world, Some(&room), true, Some(767), true)
            .unwrap()
            .unwrap();
        assert_eq!(again, installed);
        assert_eq!(fs::metadata(&apply).unwrap().modified().unwrap(), before);

        // Phase Companion: the room is open but the datapack sleeps; the mod applies.
        let companion = write_pack(&world, Some(&room), false, Some(767), true)
            .unwrap()
            .unwrap();
        assert!(!companion.active);
        assert!(
            fs::read_to_string(&apply)
                .unwrap()
                .contains("bossbar remove phase:room")
        );
        assert!(
            fs::read_to_string(&room_json)
                .unwrap()
                .contains("\"active\": true")
        );

        // The room is closed: cleanup remembers the teams, the install time does not change.
        write_pack(&world, Some(&room), true, Some(767), true).unwrap();
        let asleep = write_pack(&world, None, false, Some(767), true)
            .unwrap()
            .unwrap();
        assert!(!asleep.active);
        assert_eq!(asleep.teams, ["ph.a00"]);
        assert_eq!(asleep.installed_unix, installed.installed_unix);
        let text = fs::read_to_string(&apply).unwrap();
        assert!(text.contains("team remove ph.a00") && !text.contains("Steve"));
        assert!(
            fs::read_to_string(&room_json)
                .unwrap()
                .contains("\"active\": false")
        );
        assert_eq!(read_state(&pack), Some(asleep));
        fs::remove_dir_all(&world).unwrap();
    }
}
