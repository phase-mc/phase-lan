// In-game room styling: header, nick colors, prefixes and silhouette highlight.
// In an open room the core maintains all state (`host.look` in the session state) — the store
// only reads it and sends commands; while there is no room, the host's settings are read from
// the profile folder. For a guest — how they look in the host's world (`guest.look`).

import { app } from "./app.svelte";
import {
  core,
  DEFAULT_LOOK,
  type GuestLook,
  type LookPlayer,
  type LookSettings,
  type LookStatus,
  type PlayerStyle,
} from "./core";
import { t } from "./i18n/index.svelte";
import { settings as prefs } from "./settings.svelte";

export const EMPTY_STYLE: PlayerStyle = { color: null, prefix: null, glow: false };

class LookStore {
  /** Saved profile settings and whose they are (while there is no room). */
  #stored = $state<{ profile: string; settings: LookSettings } | null>(null);
  /** Error of the last styling action. */
  error = $state<string | null>(null);

  constructor() {
    $effect.root(() => {
      // The profile changed or the room closed — re-read what's saved.
      $effect(() => {
        void prefs.profile;
        if (!this.status) void this.load();
      });
    });
  }

  /** Styling of the open room (`null` — no room or we are a guest). */
  get status(): LookStatus | null {
    return app.role === "host" ? (app.status?.host?.look ?? null) : null;
  }

  /** Host settings: live from the room, otherwise saved in the profile. */
  get settings(): LookSettings {
    if (this.status) return this.status.settings;
    return this.#stored?.profile === prefs.profile ? this.#stored.settings : DEFAULT_LOOK;
  }

  /** Players of the open room: the host and guests. */
  get players(): LookPlayer[] {
    return this.status?.players ?? [];
  }

  /** How this device looks in the host's world (guest only). */
  get mine(): GuestLook | null {
    return app.role === "guest" ? (app.status?.guest?.look ?? null) : null;
  }

  /** The style the host assigned to a player (without personal styles and roles). */
  styleOf(deviceId: string): PlayerStyle {
    return this.settings.styles[deviceId] ?? EMPTY_STYLE;
  }

  async load(): Promise<void> {
    const profile = prefs.profile;
    try {
      const settings = await core.roomLook(profile);
      if (profile === prefs.profile) this.#stored = { profile, settings };
    } catch {
      // No profile yet or a window without a core — default settings.
    }
  }

  async #run(action: () => Promise<void>): Promise<boolean> {
    try {
      await action();
      this.error = null;
      return true;
    } catch (e) {
      this.error = String(e);
      return false;
    }
  }

  /** Changes shared settings: whether styling is on, the title, guest highlight, slots. */
  update(patch: Partial<LookSettings>): Promise<boolean> {
    const profile = prefs.profile;
    const next = { ...this.settings, ...patch };
    return this.#run(async () => {
      const saved = await core.setRoomLook(profile, next);
      if (profile === prefs.profile) this.#stored = { profile, settings: saved };
    });
  }

  /** The host sets a player's (or their own) style: color, prefix, highlight. Free. */
  setStyle(deviceId: string, patch: Partial<PlayerStyle>): Promise<boolean> {
    const profile = prefs.profile;
    const style = { ...this.styleOf(deviceId), ...patch };
    return this.#run(async () => {
      await core.setPlayerStyle(profile, deviceId, style);
      if (!this.status) await this.load();
    });
  }

  /** A guest toggles highlight on themselves; the result comes from the host in `mine`. */
  setOwnGlow(glow: boolean): Promise<boolean> {
    return this.#run(async () => {
      if (!(await core.setOwnGlow(glow))) throw t("look.noConnection");
    });
  }

}

export const look = new LookStore();
