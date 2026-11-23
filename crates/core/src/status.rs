//! Current session state (a `watch` channel): what is happening right now.

use phase_minecraft::mods::DirSource;
use serde::Serialize;

use crate::look::LookStatus;

/// Who we are in this session.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Host,
    Guest,
}

/// Session stage.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Preparation: settings, device key, search for the world and voice ports.
    Preparing,
    /// Host: connecting to the relay and issuing a ticket. Guest: connecting to the host (including
    /// waiting while the host answers "let in?").
    Connecting,
    /// Working: the host accepts guests, the guest is connected to the host.
    Running,
    /// Guest: the connection to the host is lost, reconnecting (the game is not disconnected).
    Reconnecting,
    /// Stopping: closing connections, saving the report.
    Stopping,
    /// Stopped.
    Stopped,
    /// Stopped because of an error (see [`SessionStatus::error`]).
    Failed,
}

impl Phase {
    /// The session is finished (stopped or with an error).
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Stopped | Self::Failed)
    }
}

/// Which relays the session uses.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct RelayInfo {
    /// Own relay (`false` means the free n0 relays, for checks only; or only
    /// `127.0.0.1`, in which case `urls` is empty and `loopback_only`).
    pub custom: bool,
    pub loopback_only: bool,
    /// Own relays (own and from the host's ticket, marked).
    pub urls: Vec<String>,
    /// The relay requires a password: the friend needs the same password (the ticket does not contain it).
    pub needs_token: bool,
    /// The relay has its own certificate: the friend needs the same certificate file.
    pub needs_ca: bool,
}

/// Where the host's voice port came from.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VoicePortInfo {
    pub port: u16,
    /// Explanation for the user: "Simple Voice Chat — found in the game log…".
    pub description: String,
}

/// How the host picks voice ports.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VoiceMode {
    Off,
    /// Ports are set manually; the program does not check who holds them.
    Manual,
    /// Found automatically (game log + game process ports) + fallback ones.
    Auto,
}

/// Host voice: which ports are forwarded and why.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VoicePlan {
    pub mode: VoiceMode,
    pub ports: Vec<VoicePortInfo>,
    /// Hints (game log not found, game process not identified…).
    pub hints: Vec<String>,
}

/// A guest connected to the host's room.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GuestPeer {
    /// The guest's device ID: access was granted by it.
    pub device_id: String,
    /// The in-game nickname the guest gave (`None` means an old program version without a nickname).
    /// The nickname is a label for convenience: the guest chooses it themselves.
    pub nickname: Option<String>,
    /// How many connections are open with this device (usually one).
    pub connections: u32,
}

/// Room: how many seats it has.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct RoomStatus {
    /// How many players the room holds including the host (0 means the host did not say).
    pub max_players: u32,
}

/// Mod distribution by the host.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct HostModsStatus {
    /// Distribution is on.
    pub sharing: bool,
    /// The `mods` folder distribution comes from.
    pub dir: Option<String>,
    /// Where the folder came from: the running game, the user's choice or the standard one.
    pub dir_source: Option<DirSource>,
    /// How many mods guests get and their total size.
    pub files: u32,
    pub total_size: u64,
    /// How many mods from the folder the host does not distribute (unticked).
    pub excluded: u32,
    /// Mods that cannot be distributed: an invalid file name.
    pub skipped: Vec<String>,
    /// How many files and bytes guests downloaded straight from this PC during the session.
    pub sent_files: u32,
    pub sent_bytes: u64,
    /// Why distribution does not work (folder not found, too many mods).
    pub error: Option<String>,
}

/// Mod sync stage for the guest.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModSyncState {
    /// Mods are not needed: the host does not distribute them (or has an old program version).
    NotRequired,
    /// Checking: getting the host's list and scanning your own folder.
    Checking,
    /// The check showed a difference: waiting for "Sync" or "Skip".
    NeedsSync,
    /// Downloading.
    Syncing,
    /// Mods match the host's mods.
    Done,
    /// The guest decided to join without mods (or returned their own mods from the backup).
    Skipped,
    /// Failed (see [`ModSyncStatus::error`]); can be retried.
    Failed,
}

/// Where a mod is downloaded from.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModSource {
    /// From Modrinth servers: the host's channel is not busy.
    Modrinth,
    /// Directly from the host, through the tunnel.
    P2p,
}

/// Mod download progress.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModProgress {
    /// The file being downloaded now.
    pub file: String,
    /// Its number (from one) and how many files in total.
    pub file_index: u32,
    pub files_total: u32,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub source: ModSource,
}

/// Mod sync for the guest.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ModSyncStatus {
    pub state: ModSyncState,
    /// The host can distribute mods (it has a new program version): the check can be repeated.
    pub host_shares: bool,
    /// Sync starts by itself after connecting.
    pub auto: bool,
    /// The guest's `mods` folder.
    pub dir: Option<String>,
    pub dir_source: Option<DirSource>,
    /// How many mods the host distributes and their total size.
    pub required: u32,
    pub required_bytes: u64,
    /// What the guest is missing.
    pub missing: u32,
    pub missing_bytes: u64,
    /// The first names of the missing mods (for display).
    pub missing_names: Vec<String>,
    /// The guest's extra mods: during sync they move to the backup.
    pub extra: u32,
    pub extra_names: Vec<String>,
    pub progress: Option<ModProgress>,
    /// Result of the last sync: how many mods were downloaded from Modrinth and from the host,
    /// how many of the guest's own mods were moved to the backup.
    pub from_modrinth: u32,
    pub from_host: u32,
    pub backed_up: u32,
    /// How many of the guest's own mods are in the backup.
    pub backup_files: u32,
    /// There is something to return with the "Restore my mods" button.
    pub can_restore: bool,
    /// Mods changed while the game is running: it will load them only after a restart.
    pub needs_mc_restart: bool,
    pub error: Option<String>,
}

impl ModSyncStatus {
    pub(crate) fn new(state: ModSyncState, host_shares: bool, auto: bool) -> Self {
        Self {
            state,
            host_shares,
            auto,
            dir: None,
            dir_source: None,
            required: 0,
            required_bytes: 0,
            missing: 0,
            missing_bytes: 0,
            missing_names: Vec::new(),
            extra: 0,
            extra_names: Vec::new(),
            progress: None,
            from_modrinth: 0,
            from_host: 0,
            backed_up: 0,
            backup_files: 0,
            can_restore: false,
            needs_mc_restart: false,
            error: None,
        }
    }
}

/// The host is ready: the ticket is issued, guests can connect.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostReady {
    /// Ticket for a friend (one line).
    pub ticket: String,
    /// World name for friends.
    pub motd: String,
    /// Game port on this PC (`None` means only the channel check works).
    pub game_port: Option<u16>,
    /// Game version from the server's reply.
    pub game_version: Option<String>,
    pub voice: VoicePlan,
    pub relay: RelayInfo,
    /// How many devices are already allowed.
    pub allowed_devices: usize,
    /// Ask about new devices (`false` means let in only allowed ones).
    pub asks_new_devices: bool,
    /// Who is in the room now (in order of connection). Guests' path and ping go as events
    /// ([`phase_tunnel::events::TunnelEvent`]): they change too often.
    pub guests: Vec<GuestPeer>,
    /// Mod distribution to guests.
    pub mods: HostModsStatus,
    /// Room seats and plan.
    pub room: RoomStatus,
    /// Room styling in the game: header, nickname colors, prefixes, highlight.
    pub look: LookStatus,
}

impl HostReady {
    /// A guest connected (another connection of the same device: the counter grows).
    pub(crate) fn guest_connected(&mut self, device_id: &str, nickname: Option<&str>) {
        match self.guests.iter_mut().find(|g| g.device_id == device_id) {
            Some(guest) => {
                guest.connections += 1;
                if nickname.is_some() {
                    guest.nickname = nickname.map(str::to_string);
                }
            }
            None => self.guests.push(GuestPeer {
                device_id: device_id.to_string(),
                nickname: nickname.map(str::to_string),
                connections: 1,
            }),
        }
    }

    /// A guest gave a nickname or changed it.
    pub(crate) fn guest_renamed(&mut self, device_id: &str, nickname: &str) {
        if let Some(guest) = self.guests.iter_mut().find(|g| g.device_id == device_id) {
            guest.nickname = Some(nickname.to_string());
        }
    }

    /// A guest connection closed; the last one means the guest leaves the list.
    pub(crate) fn guest_disconnected(&mut self, device_id: &str) {
        if let Some(at) = self.guests.iter().position(|g| g.device_id == device_id) {
            if self.guests[at].connections > 1 {
                self.guests[at].connections -= 1;
            } else {
                self.guests.remove(at);
            }
        }
    }
}

/// A guest is connected: the host's world is available on this PC.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GuestReady {
    pub host_id: String,
    /// The host's in-game nickname (`None` means the host has an old program version without a nickname).
    pub host_nickname: Option<String>,
    /// The host's world name.
    pub motd: String,
    /// Address for Minecraft: "Direct Connection" → this address.
    pub local_addr: String,
    /// Name in the "Local network" list (`None` means the announcement is off).
    pub lan_name: Option<String>,
    /// The host's game version.
    pub game_version: Option<String>,
    /// Open voice ports on `127.0.0.1`.
    pub voice_ports: Vec<u16>,
    pub relay: RelayInfo,
    /// Mod sync with the host.
    pub mods: ModSyncStatus,
    /// Seats in the room (`None` means the host has an old program version that does not report them).
    pub room: Option<RoomStatus>,
    /// How this device looks in the host's world (`None` means the host has a program version without
    /// styling or the answer has not arrived yet).
    pub look: Option<GuestLook>,
}

/// A guest's look in the host's world. The host decides everything; the guest can only toggle the highlight
/// for themselves if the host allowed it ([`GuestLook::can_glow`]).
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct GuestLook {
    /// Styling in the game is on at the host.
    pub enabled: bool,
    /// Nickname the game joined the world with (`None` means it has not joined a world yet).
    pub game_name: Option<String>,
    /// Nickname color: an identifier of a Phase palette color.
    pub color: Option<String>,
    /// Prefix without brackets.
    pub prefix: Option<String>,
    pub glow: bool,
    pub can_glow: bool,
}

impl From<phase_tunnel::wire::LookState> for GuestLook {
    fn from(state: phase_tunnel::wire::LookState) -> Self {
        Self {
            enabled: state.enabled,
            game_name: state.game_name,
            color: state.color,
            prefix: state.prefix,
            glow: state.glow,
            can_glow: state.can_glow,
        }
    }
}

/// The whole session state.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionStatus {
    /// Session number in this program run (grows with each new session).
    pub id: u64,
    pub role: Role,
    pub profile: String,
    pub phase: Phase,
    /// ID of this device (appears after the key is loaded).
    pub device_id: Option<String>,
    /// The in-game nickname this device is seen under on the other side (appears
    /// together with the ID).
    pub nickname: Option<String>,
    pub host: Option<HostReady>,
    pub guest: Option<GuestReady>,
    /// Why the session ended with an error.
    pub error: Option<String>,
    /// Revision number of this session's state (grows with every change). The interface
    /// receives state in different ways (a command reply, an event) and uses this number
    /// to discard stale copies.
    pub rev: u64,
}

impl SessionStatus {
    /// Applies a change and increases [`SessionStatus::rev`].
    pub(crate) fn change(&mut self, change: impl FnOnce(&mut Self)) {
        change(self);
        self.rev += 1;
    }

    pub(crate) fn new(id: u64, role: Role, profile: String) -> Self {
        Self {
            id,
            role,
            profile,
            phase: Phase::Preparing,
            device_id: None,
            nickname: None,
            host: None,
            guest: None,
            error: None,
            rev: 0,
        }
    }
}
