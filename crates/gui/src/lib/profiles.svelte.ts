// Device profiles: the list, the current profile (from settings), creating a new one.

import { core, NICKNAME_MAX, type AllowedDevice, type CoreInfo, type ProfileInfo } from "./core";
import { t } from "./i18n/index.svelte";
import { settings } from "./settings.svelte";

export const PROFILE_NAME = /^[A-Za-z0-9_-]{1,32}$/;

/** The nickname as the core will save it: no spaces at the edges, runs of spaces collapse to one. */
export function cleanNickname(raw: string): string {
  return raw.trim().replace(/\s+/g, " ");
}

/** What is wrong with the nickname (`null` — all fine). The core makes the final check. */
export function nicknameError(raw: string): string | null {
  const length = [...cleanNickname(raw)].length;
  if (length === 0) return t("nickname.empty");
  if (length > NICKNAME_MAX) return t("nickname.tooLong", { max: NICKNAME_MAX, len: length });
  return null;
}

class ProfilesStore {
  info = $state<CoreInfo | null>(null);
  list = $state<ProfileInfo[]>([]);
  /** Error loading the list. */
  error = $state<string | null>(null);
  /** The last read allowed list and whose it is. */
  #allowed = $state<{ profile: string; list: AllowedDevice[] } | null>(null);
  /** Error reading the allowed list. */
  allowedError = $state<string | null>(null);

  /** Allowed devices of the current profile (`null` — not read yet). */
  get allowed(): AllowedDevice[] | null {
    return this.#allowed?.profile === settings.profile ? this.#allowed.list : null;
  }

  /** Re-reads the list of allowed devices of the current profile. */
  async loadAllowed(): Promise<void> {
    const profile = settings.profile;
    try {
      const list = await core.allowedDevices(profile);
      // The profile may have changed while reading — a stale reply isn't needed.
      if (profile !== settings.profile) return;
      this.#allowed = { profile, list };
      this.allowedError = null;
    } catch (e) {
      if (profile !== settings.profile) return;
      this.allowedError = String(e);
    }
  }

  /** Removes a device from the current profile's quick join list and reloads the list. */
  async forget(deviceId: string): Promise<void> {
    await core.forgetDevice(settings.profile, deviceId);
    await this.loadAllowed();
  }

  get current(): ProfileInfo | undefined {
    return this.list.find((p) => p.name === settings.profile);
  }

  /** In-game nickname of the current profile — friends see you under it. Empty while profiles are being read. */
  get nickname(): string {
    return this.current?.nickname ?? "";
  }

  /** Changes the current profile's nickname. Returns the error as a string. */
  async setNickname(raw: string): Promise<string | null> {
    const invalid = nicknameError(raw);
    if (invalid) return invalid;
    try {
      const updated = await core.setProfileNickname(settings.profile, cleanNickname(raw));
      const at = this.list.findIndex((p) => p.name === updated.name);
      if (at >= 0) this.list[at] = updated;
      else await this.load();
      return null;
    } catch (e) {
      return String(e);
    }
  }

  async load(): Promise<void> {
    try {
      // core.info creates the default profile if it doesn't exist yet.
      this.info = await core.info();
      this.list = await core.listProfiles();
      this.error = null;
      if (!this.list.some((p) => p.name === settings.profile)) {
        settings.profile = this.info.default_profile;
      }
    } catch (e) {
      this.error = String(e);
    }
  }

  /** Creates a profile and makes it current. Returns the error as a string. */
  async create(name: string): Promise<string | null> {
    const clean = name.trim();
    if (!PROFILE_NAME.test(clean)) return t("profile.nameRule");
    try {
      await core.ensureProfile(clean);
      await this.load();
      settings.profile = clean;
      return null;
    } catch (e) {
      return String(e);
    }
  }
}

export const profiles = new ProfilesStore();
