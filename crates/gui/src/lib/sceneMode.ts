// Scene frame mode — a pure function of window and game state (test: sceneMode.test.ts).
//   full — every monitor frame; calm — ~30 fps; off — the requestAnimationFrame loop is stopped.

export type SceneMode = "full" | "calm" | "off";

export interface SceneInput {
  /** The window is on screen (not minimized or hidden). */
  visible: boolean;
  /** The window is focused. */
  focused: boolean;
  /** The cursor is over an inactive window (hover-wake). */
  awake?: boolean;
  /** javaw.exe (the game) is running. */
  gameRunning: boolean;
}

export function sceneMode({ visible, focused, awake = false, gameRunning }: SceneInput): SceneMode {
  if (!visible) return "off";
  if (focused) return "full";
  // Without focus while the game runs, the game needs every frame: the scene stands still, even under the cursor.
  if (gameRunning) return "off";
  return awake ? "full" : "calm";
}
