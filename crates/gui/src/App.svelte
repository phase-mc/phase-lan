<script lang="ts">
  // Frame of the control-panel window: a solid theme background, the navigation row on top (mark, tabs,
  // room, profile), the tab screen across the full width below it. "Room and guests"
  // lives in the capsule's popover, theme and settings — in the profile menu. A tab isn't tied to a session: during
  // a room or connection you can go to "Journal" or "Network" and come back.
  import { onMount } from "svelte";
  import ApprovalDialog from "./components/ApprovalDialog.svelte";
  import ErrorBar from "./components/ErrorBar.svelte";
  import LinkDialog from "./components/LinkDialog.svelte";
  import Backdrop from "./components/Backdrop.svelte";
  import TopBar from "./components/TopBar.svelte";
  import { app } from "./lib/app.svelte";
  import { freezeWhenInactive, pulseBeat } from "./lib/fx";
  import { nav } from "./lib/nav.svelte";
  import { profiles } from "./lib/profiles.svelte";
  import HostTab from "./screens/HostTab.svelte";
  import JoinTab from "./screens/JoinTab.svelte";
  import JournalTab from "./screens/JournalTab.svelte";
  import LookTab from "./screens/LookTab.svelte";
  import ModsTab from "./screens/ModsTab.svelte";
  import NetworkTab from "./screens/NetworkTab.svelte";
  import SettingsTab from "./screens/SettingsTab.svelte";

  onMount(() => {
    void app.init();
    const stopBeat = pulseBeat();
    const unfreeze = freezeWhenInactive();
    return () => {
      stopBeat();
      unfreeze();
    };
  });

  // A new session (including one already running when the window opens) — show its tab.
  let shown = 0;
  $effect(() => {
    const s = app.status;
    if (!s || !app.active || s.id === shown) return;
    shown = s.id;
    nav.go(s.role === "host" ? "host" : "join");
  });

  // The session ended — the host may have allowed new devices: re-read the profiles.
  let wasActive = false;
  $effect(() => {
    const active = app.active;
    if (wasActive && !active) {
      void profiles.load();
      void profiles.loadAllowed();
    }
    wasActive = active;
  });

  // The line under the top row appears only when content is scrolled under it.
  let scrolled = $state(false);
  // A new tab opens from the top.
  let main = $state<HTMLElement>();
  $effect(() => {
    void nav.tab;
    main?.scrollTo({ top: 0 });
  });

  const scene = $derived(nav.tab === "host" || nav.tab === "join");
</script>

<Backdrop />
<div class="shell" data-scrolled={scrolled ? "" : undefined}>
  <TopBar />
  <main
    class="main"
    id="content"
    bind:this={main}
    data-scene={scene ? "" : undefined}
    onscroll={(e) => (scrolled = e.currentTarget.scrollTop > 4)}
  >
    <ErrorBar />
    {#if scene}
      <div class="stage">
        <section class="hero" aria-labelledby={nav.tab === "host" ? "hero-title" : "join-title"}>
          {#key nav.tab}
            <div class="hero-col tab-enter">
              {#if nav.tab === "host"}
                <HostTab />
              {:else}
                <JoinTab />
              {/if}
            </div>
          {/key}
        </section>
      </div>
    {:else}
      {#key nav.tab}
        <div class="tab-enter flex grow min-h-0 flex-col">
          {#if nav.tab === "mods"}
            <ModsTab />
          {:else if nav.tab === "journal"}
            <JournalTab />
          {:else if nav.tab === "look"}
            <LookTab />
          {:else if nav.tab === "settings"}
            <SettingsTab />
          {:else}
            <NetworkTab />
          {/if}
        </div>
      {/key}
    {/if}
  </main>
</div>

<ApprovalDialog />
<LinkDialog />
