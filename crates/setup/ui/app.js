// @ts-check
// The installer window: a simple state machine without a build step or dependencies.
// All strings are taken from the dictionaries crates/setup/i18n/<code>.json.

/** @type {any} */
const tauri = /** @type {any} */ (window).__TAURI__;
const invoke = tauri.core.invoke;

/** @type {any} */
let boot;
/** The current screen and its data. */
let view = "welcome";
/** @type {any} */
let data = {};
/** Values of the first screen. */
const options = { dir: "", desktop: true, autostart: false, deleteData: false };
/** @type {any} */
let check = null;
let language = "en";
let working = false;

const viewEl = /** @type {HTMLElement} */ (document.getElementById("view"));
const langButton = /** @type {HTMLButtonElement} */ (document.getElementById("lang-button"));
const langMenu = /** @type {HTMLElement} */ (document.getElementById("lang-menu"));
const langCurrent = /** @type {HTMLElement} */ (document.getElementById("lang-current"));

// ---------------------------------------------------------------- texts

/** @param {string} key @param {Record<string, string|number>} [fill] */
function dyn(key, fill) {
  let text = boot.dictionaries[language]?.[key] ?? key;
  for (const [name, value] of Object.entries(fill ?? {})) {
    text = text.replaceAll(`{${name}}`, String(value));
  }
  return text;
}
const t = dyn;

/** @param {unknown} text */
function esc(text) {
  return String(text).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
}

/** @param {number} bytes */
function mb(bytes) {
  return Math.ceil(bytes / (1024 * 1024));
}

/** A long path: the start and end are visible, the middle is hidden. @param {string} path */
function shortPath(path, limit = 42) {
  if (path.length <= limit) return path;
  const head = Math.ceil(limit * 0.34);
  return `${path.slice(0, head)}…${path.slice(path.length - (limit - head - 1))}`;
}

/** Error text from the dictionary. @param {any} failure */
function errorText(failure) {
  const error = failure.error;
  let text = dyn(failure.key, {
    path: error.path ?? "",
    detail: error.detail ?? "",
    need: mb(error.need ?? 0),
    free: mb(error.free ?? 0),
  });
  if (error.code === "invalidPath") text += ` ${dyn(`path.${error.problem}`)}`;
  return text;
}

// ---------------------------------------------------------------- markup pieces

/** @param {string} id @param {string} text @param {string} [cls] @param {boolean} [off] */
function button(id, text, cls = "btn-primary", off = false) {
  return `<button id="${id}" class="btn ${cls}" type="button"${off ? " disabled" : ""}>${esc(text)}</button>`;
}

/** Two-line title: the second line uses the accent color, as on the site. @param {string} title */
function titleHtml(title) {
  const [first, ...rest] = title.split("\n");
  return rest.length ? `${esc(first)}\n<span class="accent">${esc(rest.join("\n"))}</span>` : esc(first);
}

/** @param {string} title @param {string} sub */
function heading(title, sub) {
  return `<h1 id="title">${titleHtml(title)}</h1><p class="sub">${esc(sub)}</p>`;
}

/** @param {string} text @param {boolean} [bad] */
function note(text, bad = false) {
  return `<p class="note${bad ? " bad" : ""}" role="status">${esc(text)}</p>`;
}

/** A row with a switch. @param {any} c */
function toggleRow(c) {
  return `<label class="option">
    <span class="option-text">
      <span class="option-title">${esc(c.title)}</span>
      <span class="option-status${c.wrap ? " wrap" : ""}">${esc(c.status)}</span>
    </span>
    <input id="${c.id}" class="switch${c.danger ? " danger" : ""}" type="checkbox"${c.on ? " checked" : ""} />
  </label>`;
}

/** A row with a folder. @param {string} title @param {string} dir @param {string} [extra] @param {string} [action] */
function folderRow(title, dir, extra = "", action = "") {
  return `<div class="option">
    <span class="option-text">
      <span class="option-title">${esc(title)}</span>
      <span class="option-status mono" title="${esc(dir)}">${esc(shortPath(dir, 38))}</span>
      ${extra ? `<span class="option-status">${esc(extra)}</span>` : ""}
    </span>
    ${action}
  </div>`;
}

// ---------------------------------------------------------------- screens

function dirNote() {
  if (!check) return "";
  if (check.problem) return note(dyn(`path.${pathKey(check.problem)}`), true);
  if (!check.writable) return note(t("dir.noWrite"), true);
  if (!check.enoughSpace) {
    return note(t("dir.noSpace", { need: mb(check.needBytes), free: mb(check.freeBytes ?? 0) }), true);
  }
  if (check.replacesForeignExe) return note(t("dir.replace"));
  return "";
}

/** @param {string} problem */
function pathKey(problem) {
  return problem;
}

function installWords() {
  const plan = boot.plan;
  const version = boot.version;
  const current = plan.existing?.version ?? "";
  switch (plan.mode) {
    case "update":
      return {
        title: t("welcome.update.title", { version }),
        sub: current ? t("welcome.update.sub", { current }) : t("welcome.updateUnknown.sub"),
        action: t("action.update"),
      };
    case "reinstall":
      return {
        title: t("welcome.reinstall.title", { version }),
        sub: t("welcome.reinstall.sub"),
        action: t("action.reinstall"),
      };
    case "downgrade":
      return {
        title: t("welcome.downgrade.title", { version }),
        sub: t("welcome.downgrade.sub", { current }),
        action: t("action.downgrade", { version }),
      };
    default:
      return {
        title: t("welcome.fresh.title"),
        sub: t("welcome.fresh.sub"),
        action: t("action.install"),
      };
  }
}

const screens = {
  welcome() {
    const words = installWords();
    const locked = boot.plan.dirLocked;
    const blocked = !boot.payloadPresent || !check?.ok;
    return `${heading(words.title, words.sub)}
      <section class="options">
        ${folderRow(
          t("card.folder"),
          options.dir,
          locked ? t("card.folderLocked") : "",
          locked ? "" : `<button id="change-dir" class="small-btn" type="button">${esc(t("action.change"))}</button>`,
        )}
        ${toggleRow({
          id: "desktop",
          on: options.desktop,
          title: t("card.desktop"),
          status: options.desktop ? t("card.desktopOn") : t("card.desktopOff"),
        })}
        ${toggleRow({
          id: "autostart",
          on: options.autostart,
          title: t("card.autostart"),
          status: options.autostart ? t("card.autostartOn") : t("card.autostartOff"),
        })}
      </section>
      ${dirNote()}
      <div class="actions">
        <span class="meta">${esc(t("meta.version", { version: "" }).trim())}
          <span class="tag">${esc(boot.version)}</span> · ${esc(t("meta.size", { size: mb(boot.sizeBytes) }))}</span>
        ${button("primary", words.action, "btn-primary", blocked)}
      </div>`;
  },

  running() {
    const closing = data.state === "closing";
    let message = "";
    if (closing) message = note(t("running.closing"));
    if (data.state === "unsupported") message = note(t("running.unsupported"));
    if (data.state === "still") message = note(t("running.still"));
    const manual = data.state === "unsupported" || data.state === "still";
    return `${heading(t("running.title"), t("running.sub"))}
      ${message}
      <div class="actions">
        ${button("secondary", t("action.back"), "btn-ghost")}
        ${button("primary", manual ? t("action.continue") : t("action.closePhase"), "btn-primary", closing)}
      </div>`;
  },

  progress() {
    const title =
      data.kind === "uninstall"
        ? t("progress.uninstall")
        : boot.plan.mode === "fresh"
          ? t("progress.install")
          : t("progress.update");
    const canCancel = data.kind !== "uninstall";
    return `<h1 id="title">${titleHtml(title)}</h1>
      <div id="percent" class="percent"></div>
      <div id="stage" class="stage" aria-live="polite"></div>
      <div id="track" class="track" role="progressbar" aria-labelledby="stage" aria-valuemin="0" aria-valuemax="100">
        <div id="bar" class="bar"></div>
      </div>
      ${canCancel ? `<div class="actions">${button("secondary", t("action.cancel"), "btn-ghost", data.cancelling)}</div>` : ""}`;
  },

  done() {
    const fresh = boot.plan.mode === "fresh";
    return `${heading(
        fresh ? t("done.install.title") : t("done.update.title", { version: boot.version }),
        fresh ? t("done.install.sub") : t("done.update.sub"),
      )}
      <div class="actions">
        ${button("secondary", t("action.close"), "btn-ghost")}
        ${button("primary", t("action.open"))}
      </div>`;
  },

  failed() {
    const code = data.failure.error.code;
    let title = t("failed.title");
    if (code === "corruptPayload") title = t("failed.corruptTitle");
    if (code === "notEnoughDisk") title = t("failed.diskTitle");
    if (code === "noWriteAccess") title = t("failed.accessTitle");
    const uninstalling = boot.mode === "uninstall";
    const fixable = !["corruptPayload", "notInstalled", "alreadyRunning"].includes(code);
    return `${heading(title, errorText(data.failure))}
      ${note(t("failed.rolledBack"))}
      <div class="actions">
        ${button("secondary", t("action.close"), "btn-ghost")}
        ${fixable ? button("primary", uninstalling ? t("action.retry") : t("action.back")) : ""}
      </div>`;
  },

  cancelled() {
    return `${heading(t("cancelled.title"), t("cancelled.sub"))}
      <div class="actions">
        ${button("secondary", t("action.close"), "btn-ghost")}
        ${button("primary", t("action.back"))}
      </div>`;
  },

  uninstall() {
    const on = options.deleteData;
    return `${heading(t("uninstall.title"), t("uninstall.sub"))}
      <section class="options">
        ${folderRow(t("uninstall.folder"), boot.uninstallDir ?? "")}
        ${toggleRow({
          id: "delete-data",
          on,
          danger: true,
          wrap: true,
          title: t("uninstall.dataLabel"),
          status: on ? t("uninstall.dataWarning") : t("uninstall.dataOff"),
        })}
      </section>
      <div class="actions">
        ${button("secondary", t("action.keep"), "btn-ghost")}
        ${button("primary", on ? t("action.uninstallAll") : t("action.uninstall"), "btn-primary", !boot.uninstallDir)}
      </div>`;
  },

  uninstalled() {
    let sub = t("uninstalled.kept", { path: boot.dataDir });
    if (data.dataDeleted) sub = t("uninstalled.removed");
    if (data.dataErrors?.length) sub = t("uninstalled.partial", { path: boot.dataDir });
    return `${heading(t("uninstalled.title"), sub)}
      <div class="actions">${button("primary", t("action.close"))}</div>`;
  },
};

// ---------------------------------------------------------------- actions

/** The actions of each screen's buttons. @type {Record<string, () => void>} */
const primaryActions = {
  welcome: () => void beginInstall(),
  running: () => void resolveRunning(),
  done: () => void openPhase(),
  failed: () => backToStart(),
  cancelled: () => backToStart(),
  uninstall: () => void beginUninstall(),
  uninstalled: () => closeWindow(),
};

/** @type {Record<string, () => void>} */
const secondaryActions = {
  running: () => {
    // We stop waiting for Phase to close; its own question about the room stays with it.
    void invoke("cancel");
    backToStart();
  },
  progress: () => void cancelWork(),
  done: () => closeWindow(),
  failed: () => closeWindow(),
  cancelled: () => closeWindow(),
  uninstall: () => closeWindow(),
};

function startScreen() {
  return boot.mode === "uninstall" ? "uninstall" : "welcome";
}

function backToStart() {
  show(startScreen());
}

function closeWindow() {
  void invoke("close_window");
}

async function refreshCheck() {
  check = await invoke("check_dir", { dir: options.dir });
  if (check.ok || !check.problem) options.dir = check.dir;
}

async function beginInstall() {
  await refreshCheck();
  if (!check.ok) {
    show("welcome");
    return;
  }
  if (await invoke("phase_running", { dir: options.dir })) {
    show("running", { state: "ask" });
    return;
  }
  startInstall();
}

/** Which Phase copies get in the way: deleting data affects any, so no folder is given. */
function blockingDir() {
  if (boot.mode !== "uninstall") return options.dir;
  return options.deleteData ? null : boot.uninstallDir;
}

function continueAfterClose() {
  if (boot.mode === "uninstall") startUninstall();
  else startInstall();
}

async function resolveRunning() {
  const dir = blockingDir();
  if (!(await invoke("phase_running", { dir }))) {
    continueAfterClose();
    return;
  }
  show("running", { state: "closing" });
  const outcome = await invoke("close_phase", { dir });
  if (view !== "running") return;
  if (outcome === "closed") continueAfterClose();
  else show("running", { state: outcome === "unsupported" ? "unsupported" : "still" });
}

function startInstall() {
  working = true;
  show("progress", { kind: "install", overall: 0, stage: "verify", cancelling: false });
  const channel = new tauri.core.Channel();
  channel.onmessage = (/** @type {any} */ event) => {
    if (event.kind === "progress") {
      data.overall = event.overall;
      data.stage = event.stage;
      paintProgress();
    } else if (event.kind === "done") {
      working = false;
      show("done");
    } else {
      working = false;
      handleFailure(event);
    }
  };
  void invoke("start_install", {
    args: { dir: options.dir, desktop: options.desktop, autostart: options.autostart },
    channel,
  });
}

/** @param {any} failure */
function handleFailure(failure) {
  const code = failure.error.code;
  if (code === "cancelled") show("cancelled");
  else if (code === "phaseRunning") show("running", { state: "ask" });
  else show("failed", { failure });
}

async function beginUninstall() {
  if (await invoke("phase_running", { dir: blockingDir() })) {
    show("running", { state: "ask" });
    return;
  }
  startUninstall();
}

function startUninstall() {
  working = true;
  show("progress", { kind: "uninstall", overall: 0.5, stage: "files" });
  const channel = new tauri.core.Channel();
  channel.onmessage = (/** @type {any} */ event) => {
    working = false;
    if (event.kind === "done") show("uninstalled", event);
    else handleFailure(event);
  };
  void invoke("start_uninstall", { deleteData: options.deleteData, channel });
}

async function cancelWork() {
  data.cancelling = true;
  render();
  await invoke("cancel");
}

async function openPhase() {
  try {
    await invoke("open_phase");
  } finally {
    closeWindow();
  }
}

async function changeDir() {
  const picked = await invoke("pick_dir", { current: options.dir });
  if (!picked) return;
  options.dir = picked;
  await refreshCheck();
  show("welcome", {}, "change-dir");
}

// ---------------------------------------------------------------- rendering

// Moon: 0 is new, 1 is full. The lit part is one path: the right half of the limb plus the
// terminator, an elliptical arc that bulges right before half (crescent) and left after it.
const MOON = { x: 200, y: 200, r: 190 };
let moonNow = 0.12;
let moonGoal = 0.12;
let moonFrame = 0;

/** @param {number} phase */
function drawMoon(phase) {
  const { x, y, r } = MOON;
  const rx = (Math.abs(1 - 2 * phase) * r).toFixed(2);
  const sweep = phase > 0.5 ? 1 : 0;
  document
    .getElementById("moon-lit")
    ?.setAttribute("d", `M${x} ${y - r}A${r} ${r} 0 0 1 ${x} ${y + r}A${rx} ${r} 0 0 ${sweep} ${x} ${y - r}Z`);
}

/** @param {number} phase */
function setMoon(phase) {
  moonGoal = Math.min(1, Math.max(0.02, phase));
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) {
    moonNow = moonGoal;
    drawMoon(moonNow);
    return;
  }
  if (moonFrame) return;
  const step = () => {
    moonNow += (moonGoal - moonNow) * 0.14;
    if (Math.abs(moonGoal - moonNow) < 0.002) moonNow = moonGoal;
    drawMoon(moonNow);
    moonFrame = moonNow === moonGoal ? 0 : requestAnimationFrame(step);
  };
  moonFrame = requestAnimationFrame(step);
}

/** Which phase the screen has. */
function screenPhase() {
  const overall = data.overall ?? 0;
  /** @type {Record<string, number>} */
  const phases = { welcome: 0.14, running: 0.3, done: 1, failed: 0.06, cancelled: 0.14, uninstall: 0.5, uninstalled: 0.02 };
  if (view === "progress") return data.kind === "uninstall" ? 1 - overall : overall;
  return phases[view] ?? 0.14;
}

function paintProgress() {
  setMoon(screenPhase());
  const stage = document.getElementById("stage");
  const percent = document.getElementById("percent");
  const bar = document.getElementById("bar");
  const track = document.getElementById("track");
  if (!stage || !percent || !bar || !track) return;
  const value = Math.round((data.overall ?? 0) * 100);
  stage.textContent = data.cancelling ? t("progress.cancelling") : dyn(`stage.${data.stage}`);
  percent.textContent = `${value}%`;
  bar.style.transform = `translateX(${value - 100}%)`;
  track.setAttribute("aria-valuenow", String(value));
}

/** Translates elements outside the screens. */
function paintChrome() {
  document.documentElement.lang = language;
  setMoon(screenPhase());
  for (const element of document.querySelectorAll("[data-i18n-label]")) {
    element.setAttribute("aria-label", dyn(element.getAttribute("data-i18n-label") ?? ""));
  }
  const current = boot.languages.find((/** @type {any} */ item) => item.code === language);
  langCurrent.textContent = current?.name ?? language;
  langButton.setAttribute("aria-label", `${t("lang.label")}: ${current?.name ?? language}`);
  for (const item of langMenu.querySelectorAll(".lang-item")) {
    item.setAttribute("aria-checked", String(item.getAttribute("data-code") === language));
  }
  // The language choice is saved at the start of the install, so it isn't offered later.
  langButton.hidden = view === "progress" || view === "done" || view === "uninstalled";
}

function bindScreen() {
  document.getElementById("primary")?.addEventListener("click", () => primaryActions[view]?.());
  document.getElementById("secondary")?.addEventListener("click", () => secondaryActions[view]?.());
  document.getElementById("change-dir")?.addEventListener("click", () => void changeDir());
  /** @type {[string, "desktop"|"autostart"|"deleteData"][]} */
  const toggles = [
    ["desktop", "desktop"],
    ["autostart", "autostart"],
    ["delete-data", "deleteData"],
  ];
  for (const [id, key] of toggles) {
    const input = /** @type {HTMLInputElement|null} */ (document.getElementById(id));
    input?.addEventListener("change", () => {
      options[key] = input.checked;
      render({ focus: id, animate: false });
    });
  }
}

/** @param {{focus?: string, animate?: boolean}} [opts] */
function render(opts = {}) {
  const screen = /** @type {Record<string, () => string>} */ (screens)[view];
  viewEl.innerHTML = screen();
  if (opts.animate !== false) {
    viewEl.getAnimations().forEach((animation) => animation.cancel());
    viewEl.style.animation = "none";
    void viewEl.offsetWidth;
    viewEl.style.animation = "";
  } else {
    viewEl.style.animation = "none";
  }
  bindScreen();
  paintChrome();
  if (view === "progress") paintProgress();
  const target = opts.focus ? document.getElementById(opts.focus) : null;
  if (target) target.focus();
  else if (opts.focus !== "") {
    // On the uninstall screen the safe choice gets focus first.
    const preferred = view === "uninstall" ? "secondary" : "primary";
    const button = /** @type {HTMLButtonElement|null} */ (
      document.getElementById(preferred) ?? document.getElementById("secondary")
    );
    if (button && !button.disabled) button.focus({ preventScroll: true });
  }
}

/** @param {string} next @param {any} [nextData] @param {string} [focus] */
function show(next, nextData = {}, focus) {
  view = next;
  data = nextData;
  render({ focus });
}

// ---------------------------------------------------------------- language menu

function buildLanguageMenu() {
  langMenu.innerHTML = boot.languages
    .map(
      (/** @type {any} */ item) =>
        `<button class="lang-item" type="button" role="menuitemradio" data-code="${esc(item.code)}" lang="${esc(item.code)}" tabindex="-1">
          <span>${esc(item.name)}</span>
          <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true"><path d="M3 7.5 5.75 10.25 11 4.5" /></svg>
        </button>`,
    )
    .join("");
  for (const item of langMenu.querySelectorAll(".lang-item")) {
    item.addEventListener("click", () => {
      chooseLanguage(item.getAttribute("data-code") ?? language);
    });
  }
  langMenu.addEventListener("toggle", (event) => {
    const open = /** @type {any} */ (event).newState === "open";
    langButton.setAttribute("aria-expanded", String(open));
    if (open) focusLanguageItem(languageItems().findIndex((el) => el.getAttribute("data-code") === language));
  });
  langMenu.addEventListener("keydown", (event) => {
    const items = languageItems();
    const index = items.indexOf(/** @type {HTMLElement} */ (document.activeElement));
    if (event.key === "ArrowDown") focusLanguageItem((index + 1) % items.length);
    else if (event.key === "ArrowUp") focusLanguageItem((index - 1 + items.length) % items.length);
    else if (event.key === "Home") focusLanguageItem(0);
    else if (event.key === "End") focusLanguageItem(items.length - 1);
    else if (event.key === "Tab") langMenu.hidePopover();
    else return;
    event.preventDefault();
  });
  langButton.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      langMenu.showPopover();
    }
  });
}

function languageItems() {
  return /** @type {HTMLElement[]} */ ([...langMenu.querySelectorAll(".lang-item")]);
}

/** @param {number} index */
function focusLanguageItem(index) {
  languageItems()[Math.max(index, 0)]?.focus();
}

/** @param {string} code */
function chooseLanguage(code) {
  language = code;
  langMenu.hidePopover();
  void invoke("set_language", { code });
  render({ focus: "lang-button", animate: false });
}

// ---------------------------------------------------------------- launch

async function start() {
  boot = await invoke("boot");
  language = boot.language;
  options.dir = boot.plan.dir;
  options.desktop = boot.plan.desktop;
  options.autostart = boot.plan.autostart;
  check = boot.check;

  buildLanguageMenu();
  drawMoon(moonNow);
  document.getElementById("win-min")?.addEventListener("click", () => void invoke("minimize_window"));
  document.getElementById("win-close")?.addEventListener("click", () => {
    if (working && view === "progress" && data.kind === "install") void cancelWork();
    else if (!working) closeWindow();
  });

  // The window is a program, not a browser tab.
  document.addEventListener("contextmenu", (event) => event.preventDefault());
  document.addEventListener("dragstart", (event) => event.preventDefault());
  document.addEventListener("keydown", (event) => {
    const reload = event.key === "F5" || ((event.ctrlKey || event.metaKey) && "rpsfgu".includes(event.key.toLowerCase()));
    if (reload) event.preventDefault();
  });

  if (boot.debug) {
    /** @type {any} */ (window).__setup = {
      /** @param {string} next @param {any} [nextData] */
      show: (next, nextData) => show(next, nextData),
      /** @param {any} patch */
      setCheck: (patch) => {
        check = { ...check, ...patch };
      },
      /** @param {any} patch */
      setPlan: (patch) => Object.assign(boot.plan, patch),
      view: () => view,
    };
  }

  // The first screen without focus: otherwise the button opens with a ring as if it had already been pressed.
  if (boot.mode === "install" && !boot.payloadPresent) {
    show("failed", { failure: { key: "error.corruptPayload", error: { code: "corruptPayload" } } }, "");
  } else {
    show(startScreen(), {}, "");
  }
  await Promise.race([document.fonts.ready, new Promise((resolve) => setTimeout(resolve, 600))]);
  requestAnimationFrame(() => void invoke("ready"));
}

void start();
