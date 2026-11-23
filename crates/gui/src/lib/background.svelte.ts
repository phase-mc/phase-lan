// Window background: one of the built-in pictures (our own voxel render), the player's own picture
// or no picture. The choice is stored in the window's localStorage; the own picture goes to Phase's data folder
// (`backgrounds/custom.webp`): the window shrinks it to 2560 px on the long side,
// compresses it to WebP and hands it to the program; the source file is no longer needed.

import { invoke } from "@tauri-apps/api/core";
import hero from "../assets/backdrops/hero.webp";
import bgManhunt from "../assets/backdrops/bg-manhunt.webp";
import bgPvp from "../assets/backdrops/bg-pvp.webp";
import bgSpeedrun from "../assets/backdrops/bg-speedrun.webp";
import bgSurvival from "../assets/backdrops/bg-survival.webp";
import bgWorld from "../assets/backdrops/bg-world.webp";
import { t, type Key } from "./i18n/index.svelte";

export interface BuiltinBackground {
  id: string;
  name: Key;
  src: string;
}

export const BUILTIN: readonly BuiltinBackground[] = [
  { id: "hero", name: "bg.hero", src: hero },
  { id: "world", name: "bg.world", src: bgWorld },
  { id: "survival", name: "bg.survival", src: bgSurvival },
  { id: "pvp", name: "bg.pvp", src: bgPvp },
  { id: "manhunt", name: "bg.manhunt", src: bgManhunt },
  { id: "speedrun", name: "bg.speedrun", src: bgSpeedrun },
];

/** The default background. */
export const DEFAULT_BACKGROUND = "hero";
/** No picture: the theme's solid color. */
export const NO_BACKGROUND = "none";
export const CUSTOM_BACKGROUND = "custom";

const KEY = "phase.background.v1";
/** We don't take a source larger than this: such files are a photo archive, not a background. */
const MAX_SOURCE = 40 * 1024 * 1024;
/** Long side after shrinking: the window needs no more. */
const MAX_SIDE = 2560;
/** The finished file is no larger than this. */
const MAX_RESULT = 1.5 * 1024 * 1024;
const TYPES = ["image/png", "image/jpeg", "image/webp"];

function readChoice(): string {
  try {
    const saved = localStorage.getItem(KEY);
    if (saved && (saved === NO_BACKGROUND || saved === CUSTOM_BACKGROUND || BUILTIN.some((b) => b.id === saved))) {
      return saved;
    }
  } catch {
    // Storage is unavailable — the default background.
  }
  return DEFAULT_BACKGROUND;
}

function toBlob(canvas: HTMLCanvasElement, quality: number): Promise<Blob> {
  return new Promise((resolve, reject) =>
    canvas.toBlob((blob) => (blob ? resolve(blob) : reject(new Error("toBlob"))), "image/webp", quality),
  );
}

/** Shrinks the picture and compresses it to WebP no larger than [`MAX_RESULT`]. */
async function compress(file: File): Promise<Blob> {
  const bitmap = await createImageBitmap(file);
  try {
    const scale = Math.min(1, MAX_SIDE / Math.max(bitmap.width, bitmap.height));
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.round(bitmap.width * scale));
    canvas.height = Math.max(1, Math.round(bitmap.height * scale));
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("canvas");
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    let quality = 0.86;
    let blob = await toBlob(canvas, quality);
    while (blob.size > MAX_RESULT && quality > 0.4) {
      quality -= 0.08;
      blob = await toBlob(canvas, quality);
    }
    if (blob.size > MAX_RESULT) throw new Error("size");
    return blob;
  } finally {
    bitmap.close();
  }
}

class BackgroundStore {
  choice = $state<string>(readChoice());
  /** Address of the own picture (blob:) or `null` if there is none. */
  customUrl = $state<string | null>(null);
  busy = $state(false);
  error = $state<string | null>(null);

  #loaded = false;

  /** The picture the window shows (`null` — no picture). */
  get src(): string | null {
    if (this.choice === NO_BACKGROUND) return null;
    if (this.choice === CUSTOM_BACKGROUND) return this.customUrl ?? hero;
    return BUILTIN.find((b) => b.id === this.choice)?.src ?? hero;
  }

  get isDefault(): boolean {
    return this.choice === DEFAULT_BACKGROUND;
  }

  async init(): Promise<void> {
    if (this.#loaded) return;
    this.#loaded = true;
    try {
      const bytes = await invoke<ArrayBuffer>("load_background");
      if (bytes.byteLength > 0) this.#setCustom(new Blob([bytes], { type: "image/webp" }));
    } catch {
      // No own picture — no big deal.
    }
    if (this.choice === CUSTOM_BACKGROUND && !this.customUrl) this.select(DEFAULT_BACKGROUND);
  }

  #setCustom(blob: Blob | null): void {
    if (this.customUrl) URL.revokeObjectURL(this.customUrl);
    this.customUrl = blob ? URL.createObjectURL(blob) : null;
  }

  select(id: string): void {
    this.choice = id;
    this.error = null;
    try {
      localStorage.setItem(KEY, id);
    } catch {
      // Not saved — the choice holds until restart.
    }
  }

  /** An own picture from disk: validation, compression, saving to the data folder. */
  async upload(file: File): Promise<void> {
    this.error = null;
    if (!TYPES.includes(file.type)) {
      this.error = t("bg.errType");
      return;
    }
    if (file.size > MAX_SOURCE) {
      this.error = t("bg.errBig");
      return;
    }
    this.busy = true;
    try {
      let blob: Blob;
      try {
        blob = await compress(file);
      } catch {
        this.error = t("bg.errRead");
        return;
      }
      await invoke("save_background", new Uint8Array(await blob.arrayBuffer()));
      this.#setCustom(blob);
      this.select(CUSTOM_BACKGROUND);
    } catch (e) {
      this.error = String(e);
    } finally {
      this.busy = false;
    }
  }

  /** Deletes the own picture; if it was selected — the default background returns. */
  async removeCustom(): Promise<void> {
    this.error = null;
    try {
      await invoke("delete_background");
      this.#setCustom(null);
      if (this.choice === CUSTOM_BACKGROUND) this.select(DEFAULT_BACKGROUND);
    } catch (e) {
      this.error = String(e);
    }
  }

  reset(): void {
    this.select(DEFAULT_BACKGROUND);
  }
}

export const background = new BackgroundStore();
