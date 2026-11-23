// Shared pulse of the live status dots (.dot-pulse). Instead of a separate CSS animation on every
// dot — one beat for the whole window: the data-beat attribute on <html> flips twice a second,
// and all dots change brightness in the same frame. A room left open all evening costs a couple
// of extra frames a second, not 144.
// While no live dot is on screen or the window is minimized, the beat changes nothing.

import { invoke } from "@tauri-apps/api/core";
import { sceneMode, type SceneMode } from "./sceneMode";

/** The Phase window is visible and focused (the player hasn't switched to, say, Minecraft). */
export function isActive(): boolean {
  return !document.hidden && document.hasFocus();
}

/** Reports when the window gains or loses focus or is hidden. Returns an unsubscribe. */
export function watchActive(cb: (active: boolean) => void): () => void {
  const fire = () => cb(isActive());
  document.addEventListener("visibilitychange", fire);
  window.addEventListener("focus", fire);
  window.addEventListener("blur", fire);
  return () => {
    document.removeEventListener("visibilitychange", fire);
    window.removeEventListener("focus", fire);
    window.removeEventListener("blur", fire);
  };
}

/** The window is on screen (not minimized to the taskbar or tray), with or without focus. */
export function isVisible(): boolean {
  return !document.hidden;
}

// ---------------------------------------------------------------- game (javaw.exe)
const GAME_POLL_MS = 2000;
let gameRunning = false;
const gameListeners = new Set<() => void>();
let gameStop: (() => void) | null = null;

/** Whether the game (javaw.exe) is running per the last poll. Not updated while the window is focused. */
export function isGameRunning(): boolean {
  return gameRunning;
}

async function pollGame() {
  let now = false;
  try {
    now = await invoke<boolean>("game_running");
  } catch {
    // Outside Tauri (vite in a browser) or a polling failure: assume there is no game.
  }
  if (now === gameRunning) return;
  gameRunning = now;
  gameListeners.forEach((cb) => cb());
}

/** Polls every 2 s — only while the window is visible and unfocused: when focused or minimized the game
 *  doesn't matter to the scene. Reports when `isGameRunning()` changes. Returns an unsubscribe. */
export function watchGame(cb: () => void): () => void {
  gameListeners.add(cb);
  if (!gameStop) {
    let timer = 0;
    const evaluate = () => {
      const need = isVisible() && !document.hasFocus();
      if (need && !timer) {
        void pollGame();
        timer = window.setInterval(() => void pollGame(), GAME_POLL_MS);
      } else if (!need && timer) {
        clearInterval(timer);
        timer = 0;
      }
    };
    evaluate();
    const unwatch = watchActive(evaluate);
    gameStop = () => {
      unwatch();
      clearInterval(timer);
      gameStop = null;
    };
  }
  return () => {
    gameListeners.delete(cb);
    if (!gameListeners.size) gameStop?.();
  };
}

/** State for choosing the scene frame mode (without hover-wake). */
export function currentMode(awake = false): SceneMode {
  return sceneMode({ visible: isVisible(), focused: document.hasFocus(), awake, gameRunning });
}

/** Zero-Lag: animations stand still (`html[data-paused]` in app.css) when the window is minimized or
 *  hidden, or unfocused while the game is running ("0 fps" mode). A window without focus and without a game,
 *  but visible, stays alive: next to Discord a frozen interface would look hung. */
export function freezeWhenInactive(): () => void {
  const root = document.documentElement;
  const apply = () => {
    if (currentMode() === "off") root.dataset.paused = "";
    else delete root.dataset.paused;
  };
  apply();
  const unwatch = watchActive(apply);
  const unwatchGame = watchGame(apply);
  return () => {
    unwatch();
    unwatchGame();
    delete root.dataset.paused;
  };
}

export function pulseBeat(): () => void {
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return () => {};
  const root = document.documentElement;
  let beat = 0;
  const id = setInterval(() => {
    if (currentMode() === "off" || !document.querySelector(".dot-pulse")) return;
    beat = (beat + 1) % 4;
    root.dataset.beat = String(beat);
  }, 500);
  return () => {
    clearInterval(id);
    delete root.dataset.beat;
  };
}
