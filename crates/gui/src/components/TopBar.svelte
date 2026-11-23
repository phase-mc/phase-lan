<script lang="ts">
  // The top row is the window's only navigation, like a premium web product:
  //   on the left the phase mark (a split circle with the right half shifted) and the PHASE wordmark;
  //   in the center open text tabs, the active one highlighted by a sliding capsule;
  //   on the right room and guests, profile (nickname; ID, theme and plan — in its menu) and window buttons.
  // Ctrl+1…5 — switch without the mouse (except question dialogs), ←/→ arrows move focus across tabs.
  // The tab of the running session has a dot; on "Mods" — amber, if the guest must decide about mods.
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { t } from "../lib/i18n/index.svelte";
  import { nav, TABS, type Tab } from "../lib/nav.svelte";
  import ProfileCapsule from "./ProfileCapsule.svelte";
  import RoomCapsule from "./RoomCapsule.svelte";

  /** Which tab the session runs on: the dot lights there. */
  const liveTab = $derived<Tab | null>(
    app.role === "host" ? "host" : app.role === "guest" ? "join" : null,
  );
  const live = $derived(app.status?.phase === "running");

  let items = $state<Record<string, HTMLButtonElement>>({});
  let bar = $state<HTMLElement>();
  let glider = $state({ x: 0, w: 0 });
  /** The capsule takes its first position without a transition — afterwards it slides. */
  let ready = $state(false);
  let maximized = $state(false);

  function measure() {
    const b = items[nav.tab];
    if (b) glider = { x: b.offsetLeft, w: b.offsetWidth };
  }

  $effect(() => {
    void nav.tab;
    measure();
  });

  onMount(() => {
    // Tab widths depend on the font and window: re-measure on any resize.
    const ro = new ResizeObserver(() => measure());
    if (bar) ro.observe(bar);
    void document.fonts?.ready.then(measure);
    const raf = requestAnimationFrame(() => (ready = true));
    let unlisten: (() => void) | undefined;
    if (native) {
      const w = getCurrentWindow();
      const sync = () => void w.isMaximized().then((m) => (maximized = m));
      sync();
      void w.onResized(sync).then((u) => (unlisten = u));
    }
    return () => {
      unlisten?.();
      ro.disconnect();
      cancelAnimationFrame(raf);
    };
  });

  function onkeydown(event: KeyboardEvent) {
    if (event.key !== "ArrowRight" && event.key !== "ArrowLeft") return;
    const order = TABS.map((tab) => items[tab.id]);
    const at = order.indexOf(document.activeElement as HTMLButtonElement);
    if (at < 0) return;
    event.preventDefault();
    const step = event.key === "ArrowRight" ? 1 : -1;
    order[(at + step + order.length) % order.length]?.focus();
  }

  // A window without a system frame: the window buttons are our own. Outside Tauri (a normal browser) there are none.
  const native = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

  function win(action: "minimize" | "toggleMaximize" | "close") {
    void getCurrentWindow()[action]();
  }

  function onwindowkey(event: KeyboardEvent) {
    // Ctrl+1…5. While a question dialog is open, tabs are not switched.
    if (!event.ctrlKey || event.altKey || event.shiftKey || event.metaKey) return;
    if (document.querySelector("dialog[open]")) return;
    const tab = TABS.find((entry) => entry.key === event.key);
    if (!tab) return;
    event.preventDefault();
    nav.go(tab.id);
  }
</script>

<svelte:window onkeydown={onwindowkey} />

<header class="topbar" data-tauri-drag-region>
  <span class="brand" aria-label="Phase" data-tauri-drag-region>
    <!-- The same geometry as in icons/src/phase-mark.svg (the tools/icons generator). -->
    <svg width="26" height="26" viewBox="0 0 26 26" aria-hidden="true" class="brand-mark">
      <path class="m-left" d="M12 2.5A9 9 0 0 0 12 20.5Z" />
      <path class="m-right" d="M14 5.5A9 9 0 0 1 14 23.5Z" />
    </svg>
    <span aria-hidden="true">PHASE</span>
  </span>

  <nav class="tabs" aria-label={t("topbar.sections")} bind:this={bar} data-tauri-drag-region>
    <span
      class="tab-glider"
      data-ready={ready ? "" : undefined}
      style:width="{glider.w}px"
      style:transform="translateX({glider.x}px)"
      aria-hidden="true"
    ></span>
    {#each TABS as tab (tab.id)}
      <button
        type="button"
        class="tab"
        bind:this={items[tab.id]}
        aria-current={nav.tab === tab.id ? "page" : undefined}
        aria-label="{t(tab.crumb)}. Ctrl+{tab.key}"
        onclick={() => nav.go(tab.id)}
        {onkeydown}
      >
        {t(tab.short)}
        {#if liveTab === tab.id}
          <span class="dot size-1.5 {live ? 'dot-live' : 'dot-busy'} dot-pulse" aria-hidden="true"></span>
        {:else if tab.id === "mods" && app.modsWaiting}
          <span class="dot dot-warn size-1.5" aria-hidden="true"></span>
        {/if}
        <span class="tab-key" aria-hidden="true">{t(tab.crumb)}<span class="kbd">Ctrl+{tab.key}</span></span>
      </button>
    {/each}
  </nav>

  <div class="topbar-end" data-tauri-drag-region>
    <RoomCapsule />
    <ProfileCapsule />
    {#if native}
      <div class="win-btns" role="group" aria-label={t("topbar.window")}>
        <button type="button" class="win-btn win-min" aria-label={t("topbar.minimize")} onclick={() => win("minimize")}>
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M1.5 8.5h9" /></svg>
        </button>
        <button
          type="button"
          class="win-btn win-max"
          aria-label={maximized ? t("topbar.restore") : t("topbar.maximize")}
          onclick={() => win("toggleMaximize")}
        >
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
            {#if maximized}
              <rect x="1.5" y="3.5" width="7" height="7" rx="2.5" />
              <path d="M4 1.5h4a2.5 2.5 0 0 1 2.5 2.5v4" />
            {:else}
              <path d="M1.5 5V4A2.5 2.5 0 0 1 4 1.5h1M7 1.5h1A2.5 2.5 0 0 1 10.5 4v1M10.5 7v1A2.5 2.5 0 0 1 8 10.5H7M5 10.5H4A2.5 2.5 0 0 1 1.5 8V7" />
            {/if}
          </svg>
        </button>
        <button type="button" class="win-btn win-close" aria-label={t("topbar.close")} onclick={() => win("close")}>
          <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.5 2.5l7 7M9.5 2.5l-7 7" /></svg>
        </button>
      </div>
    {/if}
  </div>
</header>

<style>
  /* The "phase shift" mark: the circle is cut vertically, the right half lower than the left. Two
     flat fills, no outlines or gradients. Dark theme — an icy core and electric blue,
     light — navy and sapphire. */
  .m-left {
    fill: #e0f2fe;
  }

  .m-right {
    fill: #38bdf8;
  }

  /* In daytime on utility tabs (light canvas) — navy and sapphire; on the world picture the mark
     stays light in both themes. */
  :global([data-theme="light"] .shell:not(.on-art)) .m-left {
    fill: #1e3a8a;
  }

  :global([data-theme="light"] .shell:not(.on-art)) .m-right {
    fill: #0284c7;
  }
</style>
