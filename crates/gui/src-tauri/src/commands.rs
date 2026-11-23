//! IPC commands the window calls from JavaScript (`invoke("name", {...})`).
//!
//! All commands are async or fast: Tauri runs async commands on background
//! threads, so the window thread never waits on the network or disk. Blocking calls (reading the key via
//! DPAPI, enumerating profiles) go through `spawn_blocking`.
//!
//! Request and response fields are snake_case, like the core's JSON (`SessionStatus` etc.);
//! the window-side types are in `src/lib/core.ts`. Errors are returned as a string — the window shows
//! it as is.

use std::{path::PathBuf, str::FromStr, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use phase_core::{
    DEFAULT_PROFILE, GamePort, GuestMods, HostMods, HostOptions, HostVoice, JoinOptions,
    LookCommand, LookSettings, ModCommand, ModrinthSource, ModsFolder, NetOptions, PendingApproval,
    PlayerStyle, Role, Session, SessionStatus, logs::LogLine, look, profiles::ProfileInfo,
};
use phase_minecraft::{
    lan::AnnounceMode,
    mods::{self as mc_mods, DirSource},
};
use phase_tunnel::{
    EndpointTicket,
    endpoint::{self, NetworkMode},
    identity,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// How often the core sends a ping: the window shows it "live", so more often than the console.
const STATUS_EVERY: Duration = Duration::from_secs(2);

fn to_message(err: anyhow::Error) -> String {
    format!("{err:#}")
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|err| {
            anyhow!(phase_lang::tr!(
                format!("фоновая задача аварийно завершилась: {err}"),
                format!("a background task crashed: {err}")
            ))
        })?
}

/// A recent world for the "World" card on the host screen.
#[derive(serde::Serialize)]
pub struct RecentWorldDto {
    name: String,
    folder: String,
    open: bool,
    opened_ms: u64,
    /// `data:image/png;base64,…` or `null`.
    icon: Option<String>,
}

/// Base64 (standard alphabet, with `=`) for world icons: no crate needed for this.
fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Recent worlds (open ones first, then by last-opened time) with icons.
#[tauri::command]
pub async fn recent_worlds(limit: usize) -> CmdResult<Vec<RecentWorldDto>> {
    blocking(move || {
        Ok(phase_minecraft::world::recent_worlds(limit.min(50))
            .into_iter()
            .map(|world| RecentWorldDto {
                icon: world
                    .icon
                    .map(|png| format!("data:image/png;base64,{}", base64(&png))),
                name: world.name,
                folder: world.folder,
                open: world.open,
                opened_ms: world.opened_ms,
            })
            .collect())
    })
    .await
    .map_err(to_message)
}

/// Whether the game (`javaw.exe`) is running. The window asks every 2 s while unfocused: with
/// the game running the scene freezes completely (0 fps). A polling error means "no game".
#[tauri::command]
pub async fn game_running() -> bool {
    blocking(|| Ok(phase_netstat::process::is_running("javaw.exe")?))
        .await
        .unwrap_or(false)
}

/// Core information for the window.
#[derive(Debug, Serialize)]
pub struct CoreInfo {
    pub app_version: &'static str,
    pub protocol_version: u32,
    /// Data folder: `%LOCALAPPDATA%\Phase`.
    pub data_dir: String,
    pub default_profile: &'static str,
    /// This device's ID in the default profile (the key is created if missing).
    pub device_id: String,
}

/// Core connectivity check: versions, data folder, device ID.
#[tauri::command]
pub async fn core_info() -> CmdResult<CoreInfo> {
    blocking(|| {
        Ok(CoreInfo {
            app_version: phase_core::APP_VERSION,
            protocol_version: phase_core::PROTOCOL_VERSION,
            data_dir: identity::data_dir()?.display().to_string(),
            default_profile: DEFAULT_PROFILE,
            device_id: phase_core::profiles::device_id(DEFAULT_PROFILE)?,
        })
    })
    .await
    .map_err(to_message)
}

// ------------------------------------------------------------------ in-game styling
// LAN is entirely free (owner's decision, 2026-11-18): title, colors, prefixes,
// highlight, footer and slot count are available to any host, Phase Pro rights are not checked.

/// A host session is running — the styling command goes to it and applies immediately.
async fn send_look(state: &AppState, command: LookCommand) -> bool {
    state
        .with_active(|session| session.role() == Role::Host && session.look_command(command))
        .await
        .unwrap_or(false)
}

/// Profile styling settings from its folder — while there is no room. In an open room
/// the window reads them from the session state (`host.look.settings`).
#[tauri::command]
pub async fn room_look(profile: String) -> CmdResult<LookSettings> {
    blocking(move || Ok(look::load_settings(&identity::profile_dir(profile.trim())?)))
        .await
        .map_err(to_message)
}

/// The host changes room styling: whether it is on, a custom header title, footer,
/// whether guests may toggle highlight on themselves, how many slots the room has. In an open
/// room it applies on the fly, otherwise it is saved for the next one. Returns the settings
/// as saved.
#[tauri::command]
pub async fn set_room_look(
    state: State<'_, AppState>,
    profile: String,
    settings: LookSettings,
) -> CmdResult<LookSettings> {
    let settings = settings.validated()?;
    if send_look(&state, LookCommand::Settings(settings.clone())).await {
        return Ok(settings);
    }
    blocking(move || {
        let dir = identity::profile_dir(profile.trim())?;
        look::update_stored(&dir, LookCommand::Settings(settings))
    })
    .await
    .map_err(to_message)
}

/// The host sets a player's (or their own) nick color from the Phase palette, a prefix of up to 8 characters,
/// and silhouette highlight. The style is stored by device ID. In an open room it applies
/// on the fly.
#[tauri::command]
pub async fn set_player_style(
    state: State<'_, AppState>,
    profile: String,
    device_id: String,
    style: PlayerStyle,
) -> CmdResult<()> {
    let style = style.validated()?;
    let command = LookCommand::Style { device_id, style };
    if send_look(&state, command.clone()).await {
        return Ok(());
    }
    blocking(move || {
        let dir = identity::profile_dir(profile.trim())?;
        look::update_stored(&dir, command).map(|_| ())
    })
    .await
    .map_err(to_message)
}

/// A guest turns silhouette highlight on or off for themselves. The host decides: the result arrives
/// in the session state (`guest.look`). `false` — there is no connection right now.
#[tauri::command]
pub async fn set_own_glow(state: State<'_, AppState>, glow: bool) -> CmdResult<bool> {
    Ok(state
        .with_active(|session| session.set_own_glow(glow))
        .await
        .unwrap_or(false))
}

/// Existing profiles (keys are not created).
#[tauri::command]
pub async fn list_profiles() -> CmdResult<Vec<ProfileInfo>> {
    blocking(phase_core::profiles::list_profiles)
        .await
        .map_err(to_message)
}

/// The profile with this name: created if it doesn't exist yet (folder and device key).
#[tauri::command]
pub async fn ensure_profile(name: String) -> CmdResult<ProfileInfo> {
    blocking(move || phase_core::profiles::ensure_profile(name.trim()))
        .await
        .map_err(to_message)
}

/// Set the profile's in-game nickname (1 to 24 characters). Friends see the device under this
/// nickname: in the "Let them in?" question and in the guest list. Returns the updated profile.
///
/// The nickname is sent to the other side on connect, so it doesn't change during a session:
/// otherwise the window would show one nickname and the friend another.
#[tauri::command]
pub async fn set_profile_nickname(
    state: State<'_, AppState>,
    profile: String,
    nickname: String,
) -> CmdResult<ProfileInfo> {
    if state.status().await.is_some_and(|s| !s.phase.is_finished()) {
        return Err(phase_lang::tr!(
            "ник меняется, когда сессии нет: сначала остановите её",
            "the nickname can be changed only when there is no session: stop it first"
        )
        .into());
    }
    blocking(move || phase_core::profiles::set_nickname(profile.trim(), &nickname))
        .await
        .map_err(to_message)
}

/// Network and relay — the part of host and guest requests they share.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct NetRequest {
    /// `normal` (default), `relay_only` or `loopback_only` (this PC only).
    pub network: Option<String>,
    /// Own relays (`https://relay.example.com`). Empty — the n0 relay (for testing only).
    pub relays: Vec<String>,
    pub relay_token: Option<String>,
    /// Path to the relay certificate file (PEM).
    pub relay_ca: Option<String>,
    /// Room code service address (empty — built-in or `PHASE_CODES_URL`).
    pub codes: Option<String>,
}

impl NetRequest {
    /// Code service for this session: in "This PC only" mode there is no internet.
    fn codes_service(&self) -> crate::codes::Service {
        if self.network.as_deref() == Some("loopback_only") {
            return crate::codes::Service::Off;
        }
        crate::codes::service(self.codes.as_deref())
    }

    fn options(&self) -> Result<NetOptions> {
        let mode = match self.network.as_deref().unwrap_or("normal") {
            "normal" => NetworkMode::Normal,
            "relay_only" => NetworkMode::RelayOnly,
            "loopback_only" => NetworkMode::LoopbackOnly,
            other => {
                return Err(anyhow!(phase_lang::tr!(
                    format!("неизвестный режим сети «{other}»"),
                    format!("unknown network mode “{other}”")
                )));
            }
        };
        let relays = self
            .relays
            .iter()
            .map(|url| url.trim())
            .filter(|url| !url.is_empty())
            .map(endpoint::parse_relay_url)
            .collect::<Result<Vec<_>>>()?;
        Ok(NetOptions {
            mode,
            relays,
            relay_token: non_empty(&self.relay_token),
            relay_ca: non_empty(&self.relay_ca).map(PathBuf::from),
        })
    }
}

fn non_empty(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn profile_name(profile: &Option<String>) -> String {
    non_empty(profile).unwrap_or_else(|| DEFAULT_PROFILE.to_string())
}

/// Where the app window saves session reports: `%LOCALAPPDATA%\Phase\reports`.
fn reports_dir() -> Option<PathBuf> {
    identity::data_dir().ok().map(|dir| dir.join("reports"))
}

/// The "create room" request (host).
#[derive(Debug, Clone, Deserialize)]
pub struct HostRequest {
    #[serde(default)]
    pub profile: Option<String>,
    /// World port: `auto` (find an open LAN world), a number, or empty (channel check only).
    #[serde(default)]
    pub port: Option<String>,
    /// World port already found by the auto-room (instead of `port`). The window doesn't set it: a room
    /// with a found port follows the world if it is reopened on a different port.
    #[serde(skip)]
    pub found_port: Option<u16>,
    #[serde(default)]
    pub motd: Option<String>,
    /// Voice: `auto` (default), `off` or comma-separated ports.
    #[serde(default)]
    pub voice: Option<String>,
    /// Ask about new devices (default — yes).
    #[serde(default = "yes")]
    pub ask_new_devices: bool,
    #[serde(default)]
    pub net: NetRequest,
    /// Mod distribution to guests (off by default).
    #[serde(default)]
    pub mods: HostModsRequest,
}

/// Mod distribution by the host: toggle, folder, and mods that are unchecked.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HostModsRequest {
    pub share: bool,
    /// `mods` folder chosen manually (empty — find it automatically).
    pub dir: Option<String>,
    /// File names the host does not distribute.
    pub excluded: Vec<String>,
}

impl HostModsRequest {
    fn options(&self) -> HostMods {
        HostMods {
            share: self.share,
            folder: mods_folder(&self.dir),
            excluded: self.excluded.clone(),
        }
    }
}

/// Mod sync on the guest side.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GuestModsRequest {
    /// `mods` folder chosen manually (empty — find it automatically).
    pub dir: Option<String>,
    /// Sync right after connecting.
    pub auto: bool,
}

fn mods_folder(dir: &Option<String>) -> ModsFolder {
    ModsFolder {
        chosen: non_empty(dir).map(PathBuf::from),
        ..ModsFolder::default()
    }
}

/// Mod hash cache file: `%LOCALAPPDATA%\Phase\mod-hashes.json`.
fn mod_hash_cache() -> Option<PathBuf> {
    identity::data_dir()
        .ok()
        .map(|dir| dir.join("mod-hashes.json"))
}

fn yes() -> bool {
    true
}

fn host_voice(voice: &Option<String>) -> Result<HostVoice> {
    let voice = non_empty(voice).unwrap_or_else(|| "auto".into());
    match voice.to_ascii_lowercase().as_str() {
        "auto" => Ok(HostVoice::default()),
        "off" => Ok(HostVoice::Off),
        list => list
            .split(',')
            .map(|p| {
                p.trim().parse::<u16>().with_context(|| {
                    phase_lang::tr!(
                        format!("«{p}»: голосовой порт — число 1–65535"),
                        format!("“{p}”: a voice port is a number from 1 to 65535")
                    )
                })
            })
            .collect::<Result<Vec<_>>>()
            .map(HostVoice::Manual),
    }
}

/// Create a room: starts the host in the background and immediately returns the initial state.
/// After that come the `core://status` and `core://event` events.
#[tauri::command]
pub async fn start_host(
    app: AppHandle,
    state: State<'_, AppState>,
    request: HostRequest,
) -> CmdResult<SessionStatus> {
    launch_host(&app, &state, request).await
}

/// Host startup — shared by the "Create room" button and the auto-room (`autoroom.rs`).
pub async fn launch_host(
    app: &AppHandle,
    state: &AppState,
    request: HostRequest,
) -> CmdResult<SessionStatus> {
    let build = || -> Result<HostOptions> {
        let mut opts = HostOptions::new(profile_name(&request.profile));
        opts.game_port = match request.found_port {
            Some(port) => Some(GamePort::Found(port)),
            None => non_empty(&request.port)
                .map(|p| GamePort::from_str(&p).map_err(|e| anyhow!(e)))
                .transpose()?,
        };
        opts.motd = non_empty(&request.motd);
        opts.voice = host_voice(&request.voice)?;
        opts.approver = request
            .ask_new_devices
            .then(|| Arc::new(state.approvals.clone()) as _);
        opts.net = request.net.options()?;
        opts.status_every = STATUS_EVERY;
        opts.reports_dir = reports_dir();
        opts.mods = request.mods.options();
        opts.mod_hash_cache = mod_hash_cache();
        Ok(opts)
    };
    let opts = build().map_err(to_message)?;
    let codes_service = request.net.codes_service();
    let status = state
        .start(app, |bus| Session::host(opts, bus))
        .await
        .map_err(to_message)?;
    // Short room code: appears once the room is ready.
    if let Some(rx) = state.watch(status.id).await {
        crate::codes::watch_host(app.clone(), codes_service, rx);
    }
    Ok(status)
}

/// The "connect by ticket" request (guest).
#[derive(Debug, Deserialize)]
pub struct JoinRequest {
    #[serde(default)]
    pub profile: Option<String>,
    pub ticket: String,
    #[serde(default)]
    pub local_port: Option<u16>,
    /// `unicast` (default), `multicast`, `both` or `off`.
    #[serde(default)]
    pub announce: Option<String>,
    /// Forward voice (default — yes).
    #[serde(default = "yes")]
    pub voice: bool,
    #[serde(default)]
    pub net: NetRequest,
    #[serde(default)]
    pub mods: GuestModsRequest,
}

/// Connect by ticket: starts the guest in the background and immediately returns the initial state.
#[tauri::command]
pub async fn join(
    app: AppHandle,
    state: State<'_, AppState>,
    request: JoinRequest,
) -> CmdResult<SessionStatus> {
    // Room code (K7F-2QX) → ticket via the code service; a long ticket is used as is.
    let ticket = crate::codes::resolve(request.net.codes_service(), &request.ticket).await?;
    let build = || -> Result<JoinOptions> {
        let ticket = EndpointTicket::from_str(&ticket).map_err(|_| {
            anyhow!(phase_lang::tr!(
                "Не похоже на приглашение. Скопируйте его целиком, как прислал друг.",
                "That does not look like an invite. Copy it in full, exactly as your friend sent it."
            ))
        })?;
        let mut opts = JoinOptions::new(profile_name(&request.profile), ticket);
        opts.local_port = request.local_port;
        opts.announce = non_empty(&request.announce)
            .map(|a| AnnounceMode::from_str(&a).map_err(|e| anyhow!(e)))
            .transpose()?
            .unwrap_or_default();
        opts.voice = request.voice;
        opts.net = request.net.options()?;
        opts.status_every = STATUS_EVERY;
        opts.reports_dir = reports_dir();
        opts.mods = GuestMods {
            folder: mods_folder(&request.mods.dir),
            auto: request.mods.auto,
            // In "this PC only" mode there is no internet by definition — Modrinth is left alone.
            modrinth: (opts.net.mode != NetworkMode::LoopbackOnly).then(ModrinthSource::default),
        };
        opts.mod_hash_cache = mod_hash_cache();
        Ok(opts)
    };
    let opts = build().map_err(to_message)?;
    state
        .start(&app, |bus| Session::join(opts, bus))
        .await
        .map_err(to_message)
}

/// Stop the session (break the link). Waits for connections to close and the report
/// to be saved; returns the final state (`null` — there was no session).
#[tauri::command]
pub async fn stop_session(
    state: State<'_, AppState>,
    auto: State<'_, crate::autoroom::AutoRoom>,
) -> CmdResult<Option<SessionStatus>> {
    // The host closed the room itself: the auto-room no longer reopens this same world.
    if let Some(port) = state
        .status()
        .await
        .filter(|s| !s.phase.is_finished())
        .and_then(|s| s.host)
        .and_then(|h| h.game_port)
    {
        auto.dismiss(port);
    }
    Ok(state.stop().await)
}

/// Current session state (`null` — no session yet).
#[tauri::command]
pub async fn session_status(state: State<'_, AppState>) -> CmdResult<Option<SessionStatus>> {
    Ok(state.status().await)
}

/// Answer to the "let the new device in?" question. `false` — the question is already closed.
#[tauri::command]
pub fn answer_approval(state: State<'_, AppState>, request_id: u64, allow: bool) -> bool {
    state.approvals.answer(request_id, allow)
}

/// Unanswered "let them in?" questions (for example, after the window reloads).
#[tauri::command]
pub fn pending_approvals(state: State<'_, AppState>) -> Vec<PendingApproval> {
    state.approvals.pending()
}

/// The latest log lines.
#[tauri::command]
pub fn recent_logs(state: State<'_, AppState>) -> Vec<LogLine> {
    state.logs.snapshot()
}

/// An allowed device of the profile: ID and last known nickname.
#[derive(Debug, Serialize)]
pub struct AllowedDevice {
    pub device_id: String,
    /// `None` — the device never gave a nickname (old app version).
    pub nickname: Option<String>,
}

/// Who the profile lets in without the "Let them in?" question — for the "Room" popup in the top bar.
/// Read-only access to the list file: the list itself is maintained by the host (core) when it answers "let in".
#[tauri::command]
pub async fn allowed_devices(profile: String) -> CmdResult<Vec<AllowedDevice>> {
    blocking(move || {
        let file = identity::profile_dir(profile.trim())?.join(phase_core::profiles::ALLOW_FILE);
        let list = phase_tunnel::access::AccessList::load(file)?;
        Ok(list
            .devices()
            .into_iter()
            .map(|(id, nickname)| AllowedDevice {
                device_id: id.to_string(),
                nickname,
            })
            .collect())
    })
    .await
    .map_err(to_message)
}

/// Removes a device from the profile's quick join list. While this profile hosts a room, the
/// list lives in the room's memory, so the command goes to the room (otherwise its next save
/// would write the device back). Without a room the file is edited directly. `true` means the
/// device was listed.
#[tauri::command]
pub async fn forget_device(
    state: State<'_, AppState>,
    profile: String,
    device_id: String,
) -> CmdResult<bool> {
    let profile = profile.trim().to_owned();
    let id: phase_tunnel::EndpointId = device_id
        .trim()
        .parse()
        .map_err(|_| phase_lang::tr!("Неверный ID устройства", "Invalid device ID").to_string())?;
    let reply = state
        .with_active(|session| {
            (session.role() == Role::Host && session.status().profile == profile)
                .then(|| session.forget_device(id))
        })
        .await
        .flatten()
        .flatten();
    if let Some(reply) = reply {
        // The room ended without replying, so its list is gone: edit the file instead.
        if let Ok(result) = reply.await {
            return result.map_err(to_message);
        }
    }
    blocking(move || {
        let file = identity::profile_dir(&profile)?.join(phase_core::profiles::ALLOW_FILE);
        phase_tunnel::access::AccessList::load(file)?.revoke(&id)
    })
    .await
    .map_err(to_message)
}

// ------------------------------------------------------------------ mods

/// A `.jar` file in the mods folder — a list row with a checkbox.
#[derive(Debug, Serialize)]
pub struct ModRow {
    pub file_name: String,
    pub size: u64,
    /// The mod can be distributed (`false` — invalid file name: the guest would not accept it).
    pub shareable: bool,
    /// Phase Companion: needed only in the host's game, not distributed to guests.
    pub companion: bool,
}

/// The mods folder on this PC — for the "Mods" tab.
#[derive(Debug, Default, Serialize)]
pub struct ModsOverview {
    /// The `mods` folder (`null` — not found: neither a game nor a chosen folder).
    pub dir: Option<String>,
    /// Where it came from: the running game, the user's choice, or the default.
    pub source: Option<DirSource>,
    pub exists: bool,
    /// Minecraft is running now.
    pub game_running: bool,
    pub files: Vec<ModRow>,
    pub total_size: u64,
    /// How many of the user's own mods are in `mods\.phase-backup`.
    pub backup_files: usize,
    /// There is something to restore with the "Restore my mods" button.
    pub can_restore: bool,
}

/// What is in the mods folder: the `.jar` list (no hashes — that's fast) and backup state.
/// `dir` — a manually chosen folder; empty — the app looks for it itself.
#[tauri::command]
pub async fn mods_overview(dir: Option<String>) -> CmdResult<ModsOverview> {
    blocking(move || {
        let folder = mods_folder(&dir);
        let mut overview = ModsOverview {
            game_running: mc_mods::is_game_running(),
            ..ModsOverview::default()
        };
        let Some(found) = mc_mods::locate(folder.chosen.as_deref(), folder.detect_running_game)
        else {
            return Ok(overview);
        };
        let jars = mc_mods::list_jars(&found.path).with_context(|| {
            phase_lang::tr!(
                format!("не удалось прочитать папку {}", found.path.display()),
                format!("could not read the folder {}", found.path.display())
            )
        })?;
        let backup = mc_mods::backup_info(&found.path);
        overview.total_size = jars.iter().map(|jar| jar.size).sum();
        overview.files = jars
            .into_iter()
            .map(|jar| ModRow {
                shareable: phase_tunnel::mods::validate_file_name(&jar.file_name).is_ok(),
                companion: mc_mods::is_companion_jar(&jar.file_name),
                file_name: jar.file_name,
                size: jar.size,
            })
            .collect();
        overview.dir = Some(found.path.display().to_string());
        overview.source = Some(found.source);
        overview.exists = found.exists;
        overview.backup_files = backup.files;
        overview.can_restore = backup.can_restore();
        Ok(overview)
    })
    .await
    .map_err(to_message)
}

/// Native "Select folder" dialog for the `mods` folder. `null` — the user closed the window.
#[tauri::command]
pub async fn pick_mods_dir(
    window: tauri::WebviewWindow,
    current: Option<String>,
) -> CmdResult<Option<String>> {
    // Open where the folder most likely lives.
    let start = non_empty(&current)
        .map(PathBuf::from)
        .or_else(mc_mods::default_mods_dir)
        .and_then(|dir| dir.ancestors().find(|p| p.is_dir()).map(PathBuf::from));
    let mut dialog = rfd::AsyncFileDialog::new()
        .set_title(phase_lang::tr!(
            "Папка mods Minecraft",
            "Minecraft mods folder"
        ))
        .set_parent(&window);
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    Ok(dialog
        .pick_folder()
        .await
        .map(|folder| folder.path().display().to_string()))
}

/// The host changes mod distribution without closing the room. `false` — there is no room right now
/// (the settings apply on the next "Create room").
#[tauri::command]
pub async fn set_host_mods(
    state: State<'_, AppState>,
    request: HostModsRequest,
) -> CmdResult<bool> {
    let mods = request.options();
    Ok(state
        .with_active(|session| session.set_host_mods(mods))
        .await
        .unwrap_or(false))
}

/// A guest's mod action: `sync` (synchronize), `skip` (join without mods),
/// `recheck` (verify again) or `restore` (restore their own mods). The result arrives in
/// the session state (`guest.mods`). `false` — there is no connection right now.
#[tauri::command]
pub async fn mod_command(state: State<'_, AppState>, action: String) -> CmdResult<bool> {
    let command = match action.as_str() {
        "sync" => ModCommand::Sync,
        "skip" => ModCommand::Skip,
        "recheck" => ModCommand::Recheck,
        "restore" => ModCommand::Restore,
        other => {
            return Err(phase_lang::tr!(
                format!("неизвестное действие с модами «{other}»"),
                format!("unknown mods action “{other}”")
            ));
        }
    };
    Ok(state
        .with_active(|session| session.mod_command(command))
        .await
        .unwrap_or(false))
}

/// Result of "Restore my mods".
#[derive(Debug, Serialize)]
pub struct RestoreSummary {
    /// How many of the user's own mods came back from the backup.
    pub restored: usize,
    /// How many host mods were removed.
    pub removed: usize,
    /// How many remain in the backup: a file with that name is already in the folder.
    pub conflicts: usize,
    /// How many files are busy (Minecraft is running).
    pub failed: usize,
}

/// "Restore my mods" without a connection: the guest has already disconnected, but the host's mods are still in the folder.
/// During a connection the same is done by `mod_command("restore")` — with a state update.
#[tauri::command]
pub async fn restore_mods_backup(
    state: State<'_, AppState>,
    dir: Option<String>,
) -> CmdResult<RestoreSummary> {
    let guest_active = state
        .with_active(|session| session.role() == Role::Guest)
        .await
        .unwrap_or(false);
    if guest_active {
        return Err(phase_lang::tr!(
            "идёт подключение: верните моды кнопкой на карточке синхронизации",
            "a connection is in progress: restore the mods with the button on the Mod Sync card"
        )
        .into());
    }
    blocking(move || {
        let report = phase_core::modsync::restore_backup(&mods_folder(&dir))?;
        Ok(RestoreSummary {
            restored: report.restored.len(),
            removed: report.removed.len(),
            conflicts: report.conflicts.len(),
            failed: report.failed.len(),
        })
    })
    .await
    .map_err(to_message)
}

/// "Install Phase Companion": downloads the mod into the host's mods folder (see `companion.rs`).
#[tauri::command]
pub async fn install_companion(
    dir: Option<String>,
    game_version: Option<String>,
) -> CmdResult<phase_core::companion::Outcome> {
    let chosen = non_empty(&dir).map(PathBuf::from);
    phase_core::companion::install_for_host(
        &phase_core::companion::Source::default(),
        chosen.as_deref(),
        game_version.as_deref(),
    )
    .await
    .map_err(to_message)
}

/// A `phase://` link the window has not picked up yet (see `links.rs`). The window calls it on
/// startup and on the `phase://link` event; the link is handed out once.
#[tauri::command]
pub fn take_link(inbox: State<'_, crate::links::LinkInbox>) -> Option<crate::links::Link> {
    inbox.take()
}

#[cfg(test)]
mod tests {
    /// Reference strings from RFC 4648 §10: all three tail cases (0, 1 and 2 bytes).
    #[test]
    fn base64_matches_rfc_vectors() {
        for (raw, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(super::base64(raw.as_bytes()), coded);
        }
    }
}
