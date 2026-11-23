// Window settings — one reactive object for the whole app. The "Host",
// "Join" and "Network and relay" tabs read and change the same fields, so changes
// are visible everywhere at once. Every change is saved to the window's localStorage.
// The relay password and the ticket never reach the disk: they are secrets.

import {
  core,
  type GuestModsRequest,
  type HostModsRequest,
  type HostRequest,
  type NetRequest,
  type NetworkMode,
} from "./core";
import { language, parseLang, resolveLanguage, t, type Lang } from "./i18n/index.svelte";

export type PortMode = "auto" | "manual";
/** Window theme: Deep Space Navy (default) or Clean White & Sky Blue. */
export type Theme = "dark" | "light";

/** What is saved between launches. */
interface Saved {
  profile: string;
  portMode: PortMode;
  port: string;
  network: NetworkMode;
  relayUrl: string;
  relayCa: string;
  theme: Theme;
  /** Host: distribute mods to guests. */
  modsShare: boolean;
  /** Mods folder chosen manually (empty — the program looks for it itself). */
  modsDir: string;
  /** Host: files that were unchecked (client-only mods). */
  modsExcluded: string[];
  /** Guest: sync mods right after connecting. */
  modsAuto: boolean;
  /** Open the room by itself when the world is opened to the network (auto-room). */
  autoRoom: boolean;
  /** Room code service address (empty — the one built into the program). */
  codesUrl: string;
  /** Language chosen in the profile menu (`null` — never chosen: the installer or Windows decides). */
  language: Lang | null;
}

const KEY = "phase.settings.v1";
/** Key before the program was renamed (LanBridge): read once if there is no new one yet.
 *  The old record is not deleted — an old program version may still read it. */
const LEGACY_KEY = "lanbridge.settings.v1";

const DEFAULTS: Saved = {
  profile: "default",
  portMode: "auto",
  port: "25565",
  network: "normal",
  relayUrl: "",
  relayCa: "",
  theme: "dark",
  modsShare: true,
  modsDir: "",
  modsExcluded: [],
  modsAuto: false,
  autoRoom: true,
  codesUrl: "",
  language: null,
};

const PORT_MODES: readonly PortMode[] = ["auto", "manual"];
const NETWORKS: readonly NetworkMode[] = ["normal", "relay_only", "loopback_only"];
// Old and unknown values (for example "void" / "bone") are reset to "dark".
const THEMES: readonly Theme[] = ["dark", "light"];

function load(): Saved {
  let raw: Record<string, unknown> = {};
  try {
    const stored = localStorage.getItem(KEY) ?? localStorage.getItem(LEGACY_KEY);
    const parsed: unknown = JSON.parse(stored ?? "{}");
    if (parsed && typeof parsed === "object") raw = parsed as Record<string, unknown>;
  } catch {
    // Storage is unavailable or corrupted — start with default settings.
  }
  const text = (key: keyof Saved) =>
    typeof raw[key] === "string" ? (raw[key] as string) : (DEFAULTS[key] as string);
  const oneOf = <T extends string>(key: keyof Saved, allowed: readonly T[]): T =>
    allowed.includes(raw[key] as T) ? (raw[key] as T) : (DEFAULTS[key] as T);
  const flag = (key: keyof Saved) =>
    typeof raw[key] === "boolean" ? (raw[key] as boolean) : (DEFAULTS[key] as boolean);
  const excluded: unknown = raw.modsExcluded;
  return {
    modsShare: flag("modsShare"),
    modsDir: text("modsDir"),
    modsExcluded: Array.isArray(excluded)
      ? excluded.filter((name): name is string => typeof name === "string")
      : [],
    modsAuto: flag("modsAuto"),
    autoRoom: flag("autoRoom"),
    codesUrl: text("codesUrl"),
    profile: text("profile") || DEFAULTS.profile,
    portMode: oneOf("portMode", PORT_MODES),
    port: text("port"),
    network: oneOf("network", NETWORKS),
    relayUrl: text("relayUrl"),
    relayCa: text("relayCa"),
    theme: oneOf("theme", THEMES),
    language: parseLang(typeof raw.language === "string" ? raw.language : null),
  };
}

/** The theme lives as an attribute on <html>: all colors come from the app.css tokens. */
function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme;
}

class SettingsStore {
  profile = $state(DEFAULTS.profile);
  portMode = $state<PortMode>(DEFAULTS.portMode);
  port = $state(DEFAULTS.port);
  network = $state<NetworkMode>(DEFAULTS.network);
  relayUrl = $state(DEFAULTS.relayUrl);
  relayCa = $state(DEFAULTS.relayCa);
  theme = $state<Theme>(DEFAULTS.theme);
  modsShare = $state(DEFAULTS.modsShare);
  modsDir = $state(DEFAULTS.modsDir);
  modsExcluded = $state<string[]>([]);
  modsAuto = $state(DEFAULTS.modsAuto);
  autoRoom = $state(DEFAULTS.autoRoom);
  codesUrl = $state(DEFAULTS.codesUrl);
  /** The user's chosen language (`null` — never chosen: see `language.current`). */
  language = $state<Lang | null>(DEFAULTS.language);
  /** Relay password: lives while the window is open, never written to disk. */
  relayToken = $state("");

  constructor() {
    Object.assign(this, load());
    applyTheme(this.theme);
    // Language: the setting, otherwise the system's until the core replies (no flicker), then more precise.
    language.set(this.language ?? resolveLanguage(null, null, navigator.language));
    if (!this.language) void this.#detectLanguage();
    // Autosave: an effect re-reads all fields and writes them on any change.
    $effect.root(() => {
      $effect(() => {
        const saved: Saved = {
          profile: this.profile,
          portMode: this.portMode,
          port: this.port,
          network: this.network,
          relayUrl: this.relayUrl,
          relayCa: this.relayCa,
          theme: this.theme,
          modsShare: this.modsShare,
          modsDir: this.modsDir,
          modsExcluded: [...this.modsExcluded],
          modsAuto: this.modsAuto,
          autoRoom: this.autoRoom,
          codesUrl: this.codesUrl,
          language: this.language,
        };
        try {
          localStorage.setItem(KEY, JSON.stringify(saved));
        } catch {
          // Storage is unavailable — settings just won't be saved.
        }
      });
      $effect(() => applyTheme(this.theme));
      // The core speaks the same language: tray, notifications, session messages, in-game captions.
      $effect(() => {
        const code = language.current;
        core.setLanguage(code).catch(() => {
          // A window without a core (vite in a browser) — nobody to talk to.
        });
      });
      // The auto-room lives in the window's core (works from the tray too): it needs the same settings
      // as the "Create room" button.
      $effect(() => {
        const enabled = this.autoRoom;
        const request = this.hostRequest();
        core.setAutoRoom(enabled, request).catch(() => {
          // A window without a core (vite in a browser) — no auto-room.
        });
      });
    });
  }

  /** The "Create room" request with the current settings. */
  hostRequest(): HostRequest {
    return {
      profile: this.profile,
      port: this.hostPort,
      net: this.net(),
      mods: this.hostMods(),
    };
  }

  toggleTheme(): void {
    this.theme = this.theme === "dark" ? "light" : "dark";
  }

  /** Language choice in the profile menu: applied at once and remembered. */
  selectLanguage(lang: Lang): void {
    this.language = lang;
    language.set(lang);
  }

  /** No language in settings: the installer's choice, then the Windows language (we ask the core). */
  async #detectLanguage(): Promise<void> {
    try {
      const info = await core.languageInfo();
      // While waiting for the reply, the user may have chosen a language themselves.
      if (this.language) return;
      language.set(resolveLanguage(null, info.installer, info.system));
    } catch {
      // A window without a core (vite in a browser) — the browser language stays.
    }
  }

  /** Error in the port number (only in "Specify port" mode). */
  get portError(): string | null {
    if (this.portMode !== "manual") return null;
    const port = Number(this.port.trim());
    return Number.isInteger(port) && port >= 1 && port <= 65535 ? null : t("settings.portError");
  }

  /** Port for the host request: `auto` or a number. */
  get hostPort(): string {
    return this.portMode === "auto" ? "auto" : this.port.trim();
  }

  get online(): boolean {
    return this.network !== "loopback_only";
  }

  /** Short name of the network mode — for summaries. */
  get networkLabel(): string {
    if (this.network === "loopback_only") return t("network.local");
    const relay = this.relayUrl.trim() ? t("network.relayCustom") : t("network.relayN0");
    return this.network === "relay_only"
      ? t("network.relayOnly", { relay })
      : t("network.direct", { relay });
  }

  /** Mod distribution — part of the host request. */
  hostMods(): HostModsRequest {
    return {
      share: this.modsShare,
      dir: this.modsDir.trim() || undefined,
      excluded: [...this.modsExcluded],
    };
  }

  /** Mod sync — part of the guest request. */
  guestMods(): GuestModsRequest {
    return { dir: this.modsDir.trim() || undefined, auto: this.modsAuto };
  }

  /** Network and relay — the part of host and guest requests they share. */
  net(): NetRequest {
    const relay = this.relayUrl.trim();
    return {
      network: this.network,
      relays: this.online && relay ? [relay] : [],
      relay_token: this.online ? this.relayToken : undefined,
      relay_ca: this.online ? this.relayCa : undefined,
      codes: this.online ? this.codesUrl.trim() || undefined : undefined,
    };
  }
}

export const settings = new SettingsStore();
