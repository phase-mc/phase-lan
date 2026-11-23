// Bridge "window ↔ Phase core": data types and IPC command calls.
//
// The types mirror the core's JSON (Rust: crates/core, crates/tunnel/src/events.rs and
// crates/gui/src-tauri/src/commands.rs). If you change a structure in Rust, change it here too.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { fmtSize, t } from "./i18n/index.svelte";

// ---------------------------------------------------------------- event channels

export const EVENT_CHANNEL = "core://event";
export const STATUS_CHANNEL = "core://status";
export const LOG_CHANNEL = "core://log";

// ---------------------------------------------------------------- session state

export type Role = "host" | "guest";
export type Phase =
  | "preparing"
  | "connecting"
  | "running"
  | "reconnecting"
  | "stopping"
  | "stopped"
  | "failed";

export interface RelayInfo {
  custom: boolean;
  loopback_only: boolean;
  urls: string[];
  needs_token: boolean;
  needs_ca: boolean;
}

export interface VoicePlan {
  mode: "off" | "manual" | "auto";
  ports: { port: number; description: string }[];
  hints: string[];
}

/** A guest in the host's room. The guest names themselves; access is granted by `device_id`. */
export interface GuestPeer {
  device_id: string;
  nickname: string | null;
  connections: number;
}

/** Where the `mods` folder came from: the running game, the user's choice, or the default. */
export type DirSource = "running_game" | "chosen" | "default";

/** Mod distribution by the host. */
export interface HostModsStatus {
  sharing: boolean;
  dir: string | null;
  dir_source: DirSource | null;
  /** How many mods guests get and their total size. */
  files: number;
  total_size: number;
  /** Mods the host has unchecked. */
  excluded: number;
  /** Mods with invalid file names: not distributed. */
  skipped: string[];
  /** How much guests downloaded straight from this PC during the session. */
  sent_files: number;
  sent_bytes: number;
  error: string | null;
}

export type ModSyncState =
  | "not_required"
  | "checking"
  | "needs_sync"
  | "syncing"
  | "done"
  | "skipped"
  | "failed";

/** Where a mod is downloaded from: Modrinth servers or directly from the host. */
export type ModSource = "modrinth" | "p2p";

export interface ModProgress {
  file: string;
  file_index: number;
  files_total: number;
  bytes_done: number;
  bytes_total: number;
  source: ModSource;
}

/** Mod sync on the guest side. */
export interface ModSyncStatus {
  state: ModSyncState;
  /** The host can distribute mods — the check can be repeated. */
  host_shares: boolean;
  auto: boolean;
  dir: string | null;
  dir_source: DirSource | null;
  required: number;
  required_bytes: number;
  missing: number;
  missing_bytes: number;
  missing_names: string[];
  /** The guest's extra mods: moved to the backup on sync. */
  extra: number;
  extra_names: string[];
  progress: ModProgress | null;
  from_modrinth: number;
  from_host: number;
  backed_up: number;
  backup_files: number;
  can_restore: boolean;
  /** Mods changed while the game is running — Minecraft needs a restart. */
  needs_mc_restart: boolean;
  error: string | null;
}

export interface HostReady {
  ticket: string;
  motd: string;
  game_port: number | null;
  game_version: string | null;
  voice: VoicePlan;
  relay: RelayInfo;
  allowed_devices: number;
  asks_new_devices: boolean;
  /** Who is in the room now, in connection order. */
  guests: GuestPeer[];
  mods: HostModsStatus;
  room: RoomStatus;
  /** In-game room styling: header, nick colors, prefixes, highlight. */
  look: LookStatus;
}

// ---------------------------------------------------------------- in-game styling

/** A Phase palette color (Rust: crates/minecraft/src/palette.rs — same order and values). */
export interface PhaseColor {
  id: string;
  title: string;
  /** The exact brand color: the in-game prefix and the sample in the window. */
  hex: string;
  /** How the game draws a nick of this color (a team color — one of the game's 16 colors). */
  mcHex: string;
}

/** Twelve Phase colors. There is deliberately no arbitrary color (RGB wheel). */
export const PALETTE: readonly PhaseColor[] = [
  {
    id: "ice_sapphire",
    title: "Ice Sapphire",
    hex: "#38BDF8",
    mcHex: "#5555FF",
  },
  { id: "cyber_cyan", title: "Cyber Cyan", hex: "#22D3EE", mcHex: "#55FFFF" },
  { id: "crisp_white", title: "Crisp White", hex: "#F8FAFC", mcHex: "#FFFFFF" },
  {
    id: "emerald_mint",
    title: "Emerald Mint",
    hex: "#34D399",
    mcHex: "#55FF55",
  },
  { id: "amber_gold", title: "Amber Gold", hex: "#FBBF24", mcHex: "#FFAA00" },
  { id: "coral_flame", title: "Coral Flame", hex: "#FB7185", mcHex: "#FF5555" },
  {
    id: "royal_violet",
    title: "Royal Violet",
    hex: "#8B5CF6",
    mcHex: "#AA00AA",
  },
  { id: "rose_quartz", title: "Rose Quartz", hex: "#F9A8D4", mcHex: "#FF55FF" },
  {
    id: "frost_silver",
    title: "Frost Silver",
    hex: "#CBD5E1",
    mcHex: "#AAAAAA",
  },
  { id: "deep_ocean", title: "Deep Ocean", hex: "#0E7490", mcHex: "#00AAAA" },
  {
    id: "crimson_pulse",
    title: "Crimson Pulse",
    hex: "#DC2626",
    mcHex: "#AA0000",
  },
  {
    id: "obsidian_slate",
    title: "Obsidian Slate",
    hex: "#64748B",
    mcHex: "#555555",
  },
];

/** Prefix color when no custom color is chosen: Phase sapphire. */
export const DEFAULT_ACCENT = "#38BDF8";

export function findColor(id: string | null | undefined): PhaseColor | null {
  return PALETTE.find((color) => color.id === id) ?? null;
}

/** Maximum length of the prefix and of the custom header title (same as in the core). */
export const PREFIX_MAX = 8;
export const TITLE_MAX = 32;

/** What is wrong with the prefix (`null` — it's fine). Same rules as `validate_prefix` in Rust. */
export function prefixError(raw: string): string | null {
  const prefix = raw.trim();
  const length = [...prefix].length;
  if (length > PREFIX_MAX) return t("prefix.tooLong", { max: PREFIX_MAX, len: length });
  const bad = [...prefix].find((ch) => !/[\p{L}\p{N}\-_.!+*#&]/u.test(ch));
  if (bad === undefined) return null;
  return /\s/.test(bad) ? t("prefix.noSpaces") : t("prefix.badChar", { ch: bad });
}

/** A player's style: a palette color, a prefix without brackets and silhouette highlight. */
export interface PlayerStyle {
  color: string | null;
  prefix: string | null;
  glow: boolean;
}

/** Host styling settings (Rust: crates/core/src/look.rs). */
export interface LookSettings {
  enabled: boolean;
  /** Custom header title instead of PHASE. */
  title: string | null;
  /** Guests toggle highlight on themselves. */
  guest_glow: boolean;
  /** Styles the host assigned to players, by device ID. */
  styles: Record<string, PlayerStyle>;
  /**
   * Verify players' accounts on join (as the game does). `false` — let them in without checking:
   * this is done by Phase Companion, and the nick is pinned to the device. Free, independent
   * of styling.
   */
  account_check: boolean;
  /** Remove the "Powered by Phase" footer in Tab. */
  hide_footer: boolean;
  /** Room slots including the host when Phase Companion is running (2–50). Without the mod
   *  the game lets in no more than eight. */
  slots: number;
}

/** Room slots (Rust: look.rs). */
export const SLOTS = { min: 2, max: 50, default: 20, vanilla: 8 } as const;

/** How many players the room holds: without Phase Companion — no more than eight (Rust: `room_capacity`). */
export function roomCapacity(slots: number, companion: boolean): number {
  const clamped = Math.min(SLOTS.max, Math.max(SLOTS.min, Math.round(slots)));
  return companion ? clamped : Math.min(clamped, SLOTS.vanilla);
}

/** How loaded the host PC is at this slot count — for an honest hint next to the setting. */
export type SlotsLoad = "light" | "normal" | "heavy";

export function slotsLoad(slots: number): SlotsLoad {
  if (slots <= 10) return "light";
  if (slots <= 20) return "normal";
  return "heavy";
}

/** Result of installing Phase Companion (`companion.rs`). */
export type CompanionOutcome =
  | { kind: "up_to_date"; file: string }
  | { kind: "installed"; file: string; version: string; restart: boolean; removed: string[] }
  | { kind: "no_build" }
  | { kind: "unknown_loader" }
  | { kind: "unknown_game" }
  | { kind: "no_mods_folder" };

export const DEFAULT_LOOK: LookSettings = {
  enabled: true,
  title: null,
  guest_glow: true,
  styles: {},
  // By default let players in without an account check (owner's decision); the nick is pinned to the device.
  account_check: false,
  hide_footer: false,
  slots: SLOTS.default,
};

export type LookMode =
  | "off"
  | "no_world"
  | "unsupported"
  | "waiting_reload"
  | "active"
  | "companion";

/** A room player and how they look in the game. */
export interface LookPlayer {
  device_id: string;
  host: boolean;
  /** The nickname the player gave in Phase. */
  nickname: string | null;
  /** The nickname their game joined the world with (`null` — never entered the world). */
  game_name: string | null;
  /** The in-game nickname is valid for game commands. */
  styled: boolean;
  /** Prefix without brackets as it will be in the game (HOST or custom). */
  label: string | null;
  color: string | null;
  glow: boolean;
}

export interface LookStatus {
  mode: LookMode;
  /** Name of the active world's folder. */
  world: string | null;
  /** Room code in the header. */
  code: string;
  /** Header title as shown in the game. */
  title: string;
  settings: LookSettings;
  players: LookPlayer[];
  error: string | null;
}

/** A guest's view in the host's world: the host decides everything, the guest can toggle highlight on themselves. */
export interface GuestLook {
  enabled: boolean;
  game_name: string | null;
  color: string | null;
  prefix: string | null;
  glow: boolean;
  can_glow: boolean;
}

/** The room: how many players it holds including the host. */
export interface RoomStatus {
  /** 0 — the host didn't report. */
  max_players: number;
}

export interface GuestReady {
  host_id: string;
  /** Host nickname (`null` — the host has an old version without nicknames). */
  host_nickname: string | null;
  motd: string;
  local_addr: string;
  lan_name: string | null;
  game_version: string | null;
  voice_ports: number[];
  relay: RelayInfo;
  mods: ModSyncStatus;
  /** `null` — the host has an old app version that doesn't report slots. */
  room: RoomStatus | null;
  /** How this device looks in the host's world (`null` — the host has no styling or no reply yet). */
  look: GuestLook | null;
}

export interface SessionStatus {
  id: number;
  role: Role;
  profile: string;
  phase: Phase;
  device_id: string | null;
  /** The nickname this device is seen under on the other side. */
  nickname: string | null;
  host: HostReady | null;
  guest: GuestReady | null;
  error: string | null;
  /** State change number: larger — newer. */
  rev: number;
}

export function isFinished(phase: Phase): boolean {
  return phase === "stopped" || phase === "failed";
}

// ---------------------------------------------------------------- events

export type PathKind = "direct" | "relay" | "other";
export type AccessDecision =
  | "allowed"
  | "denied"
  | "already_denied"
  | "prompts_off"
  | "busy"
  | "timeout"
  | "guest_left"
  | "room_full";

export interface VoiceCounts {
  sent: number;
  received: number;
  dropped: number;
  fragmented: number;
}

export type TunnelEvent =
  | { kind: "peer_connected"; peer: string; nickname: string | null }
  | { kind: "peer_nickname"; peer: string; nickname: string }
  | { kind: "peer_game_name"; peer: string; name: string; uuid: string | null }
  | { kind: "peer_disconnected"; peer: string; reason: string }
  | { kind: "path_selected"; peer: string; path: PathKind; addr: string }
  | {
      kind: "ping";
      peer: string;
      paths: { path: PathKind; rtt_ms: number; selected: boolean }[];
    }
  | {
      kind: "access";
      peer: string;
      nickname: string | null;
      decision: AccessDecision;
    }
  | { kind: "game_unreachable"; port: number; error: string }
  | { kind: "host_lost"; reason: string }
  | { kind: "reconnect_failed"; error: string; retry_in_ms: number }
  | { kind: "host_restored" }
  | { kind: "voice_summary"; peer: string; counts: VoiceCounts }
  | { kind: "mod_sent"; peer: string; file_name: string; size: number };

export type Level = "info" | "warn" | "error";

export type SessionEvent =
  | { type: "notice"; data: { level: Level; text: string } }
  | { type: "world_search"; data: { timeout_s: number } }
  | {
      type: "world_found";
      data: { name: string; port: number; summary: string };
    }
  | {
      type: "game_checked";
      data: { port: number; summary: string | null; error: string | null };
    }
  | {
      /** The world was reopened on a different port: the room followed it (`followed`) or the port was set manually. */
      type: "world_moved";
      data: { from: number; to: number; name: string; summary: string; followed: boolean };
    }
  | {
      type: "approval_requested";
      data: { request_id: number; device_id: string; nickname: string | null };
    }
  | { type: "approval_closed"; data: { request_id: number } }
  | { type: "tunnel"; data: TunnelEvent }
  | { type: "report_saved"; data: { path: string } };

export interface LogLine {
  unix_ms: number;
  level: "error" | "warn" | "info" | "debug" | "trace";
  target: string;
  message: string;
}

// ---------------------------------------------------------------- commands

export interface CoreInfo {
  app_version: string;
  protocol_version: number;
  data_dir: string;
  default_profile: string;
  device_id: string;
}

export interface ProfileInfo {
  name: string;
  /** In-game nickname: the one set, one found in Minecraft on this PC, or the OS user name. */
  nickname: string;
  device_id: string | null;
  allowed_devices: number;
  error: string | null;
}

/** A device from the profile's allowed list (let in without the "Let them in?" question). */
export interface AllowedDevice {
  device_id: string;
  /** Last known nickname (`null` — the device never gave one). */
  nickname: string | null;
}

export interface PendingApproval {
  request_id: number;
  device_id: string;
  /** The nickname the device gave (`null` — it didn't introduce itself). */
  nickname: string | null;
}

/** Maximum nickname length (as in the core: `NICKNAME_MAX_CHARS`). */
export const NICKNAME_MAX = 24;

export type NetworkMode = "normal" | "relay_only" | "loopback_only";

export interface NetRequest {
  network?: NetworkMode;
  relays?: string[];
  relay_token?: string;
  relay_ca?: string;
  /** Room code service address (empty — built-in). */
  codes?: string;
}

export interface HostRequest {
  profile?: string;
  /** "auto", a port number or empty (channel check only). */
  port?: string;
  motd?: string;
  /** "auto" (default), "off" or comma-separated ports. */
  voice?: string;
  ask_new_devices?: boolean;
  net?: NetRequest;
  mods?: HostModsRequest;
}

/** Mod distribution by the host: toggle, folder (empty — find it automatically), unchecked boxes. */
export interface HostModsRequest {
  share: boolean;
  dir?: string;
  excluded: string[];
}

/** Mod sync on the guest side: folder and auto mode. */
export interface GuestModsRequest {
  dir?: string;
  auto: boolean;
}

export interface JoinRequest {
  profile?: string;
  ticket: string;
  local_port?: number;
  announce?: "unicast" | "multicast" | "both" | "off";
  voice?: boolean;
  net?: NetRequest;
  mods?: GuestModsRequest;
}

/** A `.jar` file in the mods folder. */
export interface ModRow {
  file_name: string;
  size: number;
  /** `false` — invalid file name: such a mod is not distributed. */
  shareable: boolean;
  /** Phase Companion: needed only in the host's game, not distributed to guests. */
  companion: boolean;
}

/** The mods folder on this PC. */
export interface ModsOverview {
  dir: string | null;
  source: DirSource | null;
  exists: boolean;
  game_running: boolean;
  files: ModRow[];
  total_size: number;
  /** How many of the user's own mods are in `mods\.phase-backup`. */
  backup_files: number;
  can_restore: boolean;
}

export interface RestoreSummary {
  restored: number;
  removed: number;
  conflicts: number;
  failed: number;
}

/** A guest's mod action. */
export type ModAction = "sync" | "skip" | "recheck" | "restore";

/** A recent Minecraft world; `icon` — `data:image/png;base64,…` or null. */
export interface RecentWorld {
  name: string;
  folder: string;
  open: boolean;
  opened_ms: number;
  icon: string | null;
}

export const core = {
  info: () => invoke<CoreInfo>("core_info"),
  recentWorlds: (limit: number) =>
    invoke<RecentWorld[]>("recent_worlds", { limit }),
  listProfiles: () => invoke<ProfileInfo[]>("list_profiles"),
  ensureProfile: (name: string) =>
    invoke<ProfileInfo>("ensure_profile", { name }),
  setProfileNickname: (profile: string, nickname: string) =>
    invoke<ProfileInfo>("set_profile_nickname", { profile, nickname }),
  roomLook: (profile: string) => invoke<LookSettings>("room_look", { profile }),
  setRoomLook: (profile: string, settings: LookSettings) =>
    invoke<LookSettings>("set_room_look", { profile, settings }),
  setPlayerStyle: (profile: string, deviceId: string, style: PlayerStyle) =>
    invoke<void>("set_player_style", { profile, deviceId, style }),
  setOwnGlow: (glow: boolean) => invoke<boolean>("set_own_glow", { glow }),
  startHost: (request: HostRequest) =>
    invoke<SessionStatus>("start_host", { request }),
  join: (request: JoinRequest) => invoke<SessionStatus>("join", { request }),
  stop: () => invoke<SessionStatus | null>("stop_session"),
  status: () => invoke<SessionStatus | null>("session_status"),
  answerApproval: (requestId: number, allow: boolean) =>
    invoke<boolean>("answer_approval", { requestId, allow }),
  pendingApprovals: () => invoke<PendingApproval[]>("pending_approvals"),
  recentLogs: () => invoke<LogLine[]>("recent_logs"),
  allowedDevices: (profile: string) =>
    invoke<AllowedDevice[]>("allowed_devices", { profile }),
  /** Remove a device from quick join (through the room if one is open). `true` if it was listed. */
  forgetDevice: (profile: string, deviceId: string) =>
    invoke<boolean>("forget_device", { profile, deviceId }),
  modsOverview: (dir: string) => invoke<ModsOverview>("mods_overview", { dir }),
  pickModsDir: (current: string) =>
    invoke<string | null>("pick_mods_dir", { current }),
  setHostMods: (request: HostModsRequest) =>
    invoke<boolean>("set_host_mods", { request }),
  modCommand: (action: ModAction) => invoke<boolean>("mod_command", { action }),
  installCompanion: (dir: string, gameVersion: string | null) =>
    invoke<CompanionOutcome>("install_companion", { dir, gameVersion }),
  restoreModsBackup: (dir: string) =>
    invoke<RestoreSummary>("restore_mods_backup", { dir }),
  takeLink:() => invoke<PhaseLink | null>("take_link"),
  setAutoRoom: (enabled: boolean, request: HostRequest) =>
    invoke<void>("set_auto_room", { enabled, request }),
  autostartEnabled: () => invoke<boolean>("autostart_enabled"),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
  roomCode: () => invoke<RoomCode | null>("room_code"),
  /** Installer choice and Windows language (Rust: `language.rs`). */
  languageInfo: () => invoke<LanguageInfo>("language_info"),
  /** The window tells the core the language: tray, notifications, session messages and in-game captions. */
  setLanguage: (code: string) => invoke<string>("set_language", { code }),
};

export interface LanguageInfo {
  installer: string | null;
  system: string;
}

/** The auto-room created the room (the world is open to the network, the window may have been in the tray). */
export const AUTO_ROOM_CHANNEL = "phase://auto-room";

export function onAutoRoom(handler: (status: SessionStatus) => void): Promise<UnlistenFn> {
  return listen<SessionStatus>(AUTO_ROOM_CHANNEL, (e) => handler(e.payload));
}

// ---------------------------------------------------------------- phase:// links

/** The "phase:// link arrived" event: the window picks up the link itself via `core.takeLink()`. */
export const LINK_CHANNEL = "phase://link";

/** Invitation link (Rust: crates/gui/src-tauri/src/links.rs). */
export type PhaseLink =
  { kind: "join"; code: string } | { kind: "ticket"; ticket: string };

export function onLink(handler: () => void): Promise<UnlistenFn> {
  return listen(LINK_CHANNEL, () => handler());
}

/** Random code characters: Crockford base32 without 0 and 1 (Rust: `phase_codes::ALPHABET`). */
const CODE_CHARS = "23456789ABCDEFGHJKMNPQRSTVWXYZ";

/** Guest input → code `K7F-2QX` (case, spaces and hyphens don't matter); `null` — not a code.
 *  Same rules as `phase_codes::normalize` in Rust. */
export function normalizeCode(text: string): string | null {
  const chars = text.replace(/[\s-]/g, "").toUpperCase();
  if (chars.length !== 6 || [...chars].some((c) => !CODE_CHARS.includes(c))) return null;
  return `${chars.slice(0, 3)}-${chars.slice(3)}`;
}

/** Room code: a random `K7F-2QX` or a named `vanilla-crew`.
 *  Same rules as `links::is_room_code` in Rust. */
export function isRoomCode(text: string): boolean {
  if (normalizeCode(text) === text) return true;
  return /^[a-z0-9-]{3,16}$/.test(text) && !/^-|-$|--/.test(text);
}

/** The host's room code (Rust: `codes.rs`). */
export interface RoomCode {
  session: number;
  state: "pending" | "ready" | "unavailable" | "off";
  code: string | null;
}

export const ROOM_CODE_CHANNEL = "phase://room-code";

export function onRoomCode(handler: (code: RoomCode | null) => void): Promise<UnlistenFn> {
  return listen<RoomCode | null>(ROOM_CODE_CHANNEL, (e) => handler(e.payload));
}

/** Size for display, localized (comma decimal and Russian units in ru, e.g. 12.3 MB / 840 KB in en). */
export const formatSize = fmtSize;

/** Localized count of mods ("1 mod", "5 mods" in en; Russian plural forms in ru). */
export function modsWord(n: number): string {
  return t("count.mods", { n });
}

/** Localized count of slots ("3 slots" in en). */
export function slotsWord(n: number): string {
  return t("count.slots", { n });
}

/** Localized count of players ("5 players" in en). */
export function playersWord(n: number): string {
  return t("count.players", { n });
}

export function onEvent(
  handler: (event: SessionEvent) => void,
): Promise<UnlistenFn> {
  return listen<SessionEvent>(EVENT_CHANNEL, (e) => handler(e.payload));
}

export function onStatus(
  handler: (status: SessionStatus) => void,
): Promise<UnlistenFn> {
  return listen<SessionStatus>(STATUS_CHANNEL, (e) => handler(e.payload));
}

export function onLog(handler: (line: LogLine) => void): Promise<UnlistenFn> {
  return listen<LogLine>(LOG_CHANNEL, (e) => handler(e.payload));
}

/** Short device ID: `#` and the first 8 characters (as in the core log). */
export function shortId(id: string): string {
  return `#${id.slice(0, 8)}`;
}

/** How to name a device in text: "Nick (#1603817a)" or just "#1603817a". */
export function peerLabel(
  id: string,
  nickname: string | null | undefined,
): string {
  return nickname ? `${nickname} (${shortId(id)})` : shortId(id);
}
