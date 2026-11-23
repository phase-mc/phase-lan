// The Phase window is a program, not a browser tab. Everything that makes WebView2 give away
// the browser is closed off here: the context menu, "Reload", "Print", "Save as", find in
// page, "Back/Forward" (keys and side mouse buttons), dragging and text selection
// with the mouse outside input fields.
//
// Second line: the same is turned off in WebView2 itself (src-tauri/src/harden.rs). This
// layer is needed because native settings don't know about side mouse buttons and
// dragging, and DevTools in a debug build is opened from here.

import { invoke } from "@tauri-apps/api/core";

/** Where text selection, pasting and dragging stay normal. */
const EDITABLE = "input, textarea";

/** Ctrl + key: browser actions the program doesn't have. Key codes, not letters:
 *  on a Russian layout the same physical key types a Cyrillic letter, but the code stays KeyR. */
const CTRL_BLOCKED = new Set([
  "KeyR", // reload
  "KeyP", // print
  "KeyS", // save as
  "KeyU", // view source
  "KeyF", // find in page
  "KeyG", // find next
  "KeyJ", // downloads
  "KeyH", // history
  "KeyO", // open file
  "KeyD", // bookmark
  "Equal", // zoom
  "Minus",
  "Digit0",
  "NumpadAdd",
  "NumpadSubtract",
  "Numpad0",
]);

/** Browser keys without modifiers. */
const PLAIN_BLOCKED = new Set([
  "F3", // find next
  "F5", // reload
  "F7", // caret browsing
  "BrowserBack",
  "BrowserForward",
  "BrowserRefresh",
  "BrowserSearch",
  "BrowserFavorites",
  "BrowserHome",
]);

function inEditable(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(EDITABLE) !== null;
}

function block(event: Event): void {
  event.preventDefault();
  event.stopPropagation();
}

function onKeyDown(event: KeyboardEvent): void {
  const ctrl = event.ctrlKey || event.metaKey;
  // DevTools: in a release build there is no command at all, in a debug build the core opens them for the window.
  if (event.code === "F12" || (ctrl && event.shiftKey && (event.code === "KeyI" || event.code === "KeyC"))) {
    block(event);
    void invoke("open_devtools").catch(() => {});
    return;
  }
  if (PLAIN_BLOCKED.has(event.code) || PLAIN_BLOCKED.has(event.key)) return block(event);
  if (ctrl && !event.altKey && CTRL_BLOCKED.has(event.code)) return block(event);
  // "Back" and "Forward" from the keyboard.
  if (event.altKey && (event.code === "ArrowLeft" || event.code === "ArrowRight")) return block(event);
}

/** Side mouse buttons (3 — "Back", 4 — "Forward"). */
function onSideButton(event: MouseEvent): void {
  if (event.button === 3 || event.button === 4) block(event);
}

/** Installs the protection once, before the interface appears. Handlers run in the capture phase:
 *  no screen can bypass them by accident. */
export function installGuard(): void {
  const capture = { capture: true };
  window.addEventListener("contextmenu", block, capture);
  window.addEventListener("keydown", onKeyDown, capture);
  for (const type of ["mousedown", "mouseup", "auxclick", "pointerdown", "pointerup"] as const) {
    window.addEventListener(type, onSideButton, capture);
  }
  // Dragging: interface elements don't "tear off", a file dropped on the window isn't
  // opened instead of the program. Text inside an input field is dragged as usual.
  window.addEventListener("dragstart", (e) => !inEditable(e.target) && block(e), capture);
  for (const type of ["dragover", "drop"] as const) {
    window.addEventListener(type, (e) => !inEditable(e.target) && e.preventDefault(), capture);
  }
  // Mouse selection — only in input fields (the same rule in app.css).
  window.addEventListener("selectstart", (e) => !inEditable(e.target) && e.preventDefault(), capture);
}
