// Mods on this PC: what is in the mods folder, which mods the host distributes, what is in the backup.
// Settings (toggle, folder, unchecked boxes, auto mode) live in the shared settings store —
// here is only what is read from disk, and actions.

import { app } from "./app.svelte";
import { core, type DirSource, type ModsOverview } from "./core";
import { t, type Key } from "./i18n/index.svelte";
import { settings } from "./settings.svelte";

/** How long to wait for new checkbox changes before rebuilding the open room's distribution. */
const PUSH_DELAY_MS = 500;

const DIR_SOURCE_KEY: Record<DirSource, Key> = {
  running_game: "modsDir.runningGame",
  chosen: "modsDir.chosen",
  default: "modsDir.default",
};

/** Where the mods folder came from — a label in the current language. */
export const dirSourceLabel = (source: DirSource): string => t(DIR_SOURCE_KEY[source]);

class ModsStore {
  /** The mods folder and its contents (`null` — not read yet). */
  overview = $state<ModsOverview | null>(null);
  loading = $state(false);
  error = $state<string | null>(null);
  /** Result of "Restore my mods" without a connection. */
  notice = $state<string | null>(null);
  restoring = $state(false);

  #requested = 0;
  #push: ReturnType<typeof setTimeout> | undefined;
  /** Distribution settings the core already knows (to avoid needless rebuilds). */
  #pushed = "";

  constructor() {
    $effect.root(() => {
      // The host changes distribution in an open room — rebuild it without a restart.
      $effect(() => {
        const request = settings.hostMods();
        const key = JSON.stringify(request);
        // No room — these settings go in the "Create room" request.
        if (app.role !== "host" || !app.status?.host) {
          this.#pushed = key;
          return;
        }
        if (key === this.#pushed) return;
        this.#pushed = key;
        clearTimeout(this.#push);
        this.#push = setTimeout(() => {
          void core.setHostMods(request).catch((e) => (app.error = String(e)));
        }, PUSH_DELAY_MS);
      });
    });
  }

  /** Names of unchecked files, lowercase (Windows is case-insensitive). */
  get excluded(): Set<string> {
    return new Set(settings.modsExcluded.map((name) => name.toLowerCase()));
  }

  /** Mods guests will receive: a valid name and the checkbox is on. */
  get shared(): { files: number; size: number } {
    const excluded = this.excluded;
    let files = 0;
    let size = 0;
    for (const file of this.overview?.files ?? []) {
      if (!file.shareable || file.companion || excluded.has(file.file_name.toLowerCase())) continue;
      files += 1;
      size += file.size;
    }
    return { files, size };
  }

  isShared(fileName: string): boolean {
    return !this.excluded.has(fileName.toLowerCase());
  }

  toggle(fileName: string, share: boolean): void {
    const key = fileName.toLowerCase();
    const rest = settings.modsExcluded.filter((name) => name.toLowerCase() !== key);
    settings.modsExcluded = share ? rest : [...rest, fileName];
  }

  /** Check or uncheck all mods of the current folder. */
  setAll(share: boolean): void {
    const here = new Set((this.overview?.files ?? []).map((f) => f.file_name.toLowerCase()));
    // Mod checkboxes from other folders are left alone.
    const others = settings.modsExcluded.filter((name) => !here.has(name.toLowerCase()));
    settings.modsExcluded = share
      ? others
      : [...others, ...(this.overview?.files ?? []).filter((f) => f.shareable).map((f) => f.file_name)];
  }

  /** Re-reads the mods folder. A reply to a stale request is dropped. */
  async refresh(): Promise<void> {
    const id = ++this.#requested;
    this.loading = true;
    try {
      const overview = await core.modsOverview(settings.modsDir.trim());
      if (id !== this.#requested) return;
      this.overview = overview;
      this.error = null;
    } catch (e) {
      if (id !== this.#requested) return;
      this.error = String(e);
    } finally {
      if (id === this.#requested) this.loading = false;
    }
  }

  /** The system folder picker. */
  async pickDir(): Promise<void> {
    try {
      const picked = await core.pickModsDir(settings.modsDir.trim() || (this.overview?.dir ?? ""));
      if (picked) settings.modsDir = picked;
    } catch (e) {
      this.error = String(e);
    }
  }

  /** "Restore my mods" when there is no connection (during a connection — a session command). */
  async restore(): Promise<void> {
    if (this.restoring) return;
    this.restoring = true;
    this.notice = null;
    try {
      const done = await core.restoreModsBackup(settings.modsDir.trim());
      const parts = [t("modsRestore.done", { restored: done.restored, removed: done.removed })];
      if (done.conflicts > 0) parts.push(t("modsRestore.conflicts", { n: done.conflicts }));
      if (done.failed > 0) parts.push(t("modsRestore.failed", { n: done.failed }));
      this.notice = parts.join(" ");
    } catch (e) {
      this.error = String(e);
    } finally {
      this.restoring = false;
      void this.refresh();
    }
  }
}

export const mods = new ModsStore();
