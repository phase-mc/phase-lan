<script lang="ts">
  // The "Host" tab — the room's control panel. On the left a heading and one blue button, on the right
  // a connection panel: a "this PC — friends" line and rows, each with its own job:
  //   before the room — "World" (an open or the last world, its icon; a click — how to find the world),
  //     "Mods for friends" (the distribution toggle; a click — the "Mods" tab) and "Styling";
  //   in the room — "Players", "World" (port, version, warning), "Mods" and "Styling".
  // During preparation — a step and a waiting bar.
  import Globe from "@lucide/svelte/icons/globe";
  import Package from "@lucide/svelte/icons/package";
  import Palette from "@lucide/svelte/icons/palette";
  import Square from "@lucide/svelte/icons/square";
  import Users from "@lucide/svelte/icons/users";
  import { onMount } from "svelte";
  import Avatar from "../components/Avatar.svelte";
  import CopyButton from "../components/CopyButton.svelte";
  import HelpPopover from "../components/HelpPopover.svelte";
  import Modal from "../components/Modal.svelte";
  import Popover from "../components/Popover.svelte";
  import { app } from "../lib/app.svelte";
  import { core, formatSize, modsWord, type Phase, type RecentWorld } from "../lib/core";
  import { t, type Key } from "../lib/i18n/index.svelte";
  import { inviteUrl } from "../lib/invite";
  import { mods } from "../lib/mods.svelte";
  import { nav } from "../lib/nav.svelte";
  import { profiles } from "../lib/profiles.svelte";
  import { settings } from "../lib/settings.svelte";

  const status = $derived(app.role === "host" ? app.status : null);
  const host = $derived(status?.host ?? null);
  /** Short room code. While it is being registered — a spot for the code; if the code service is not
   *  configured or didn't answer — the long invitation, as before. */
  const roomCode = $derived(app.roomCode);
  const useCode = $derived(roomCode?.state === "pending" || roomCode?.state === "ready");
  const shortCode = $derived(roomCode?.state === "ready" ? roomCode.code : null);
  /** Invite link for chats: the site page opens Phase with this code (only the code, see invite.ts). */
  const inviteLink = $derived(shortCode ? inviteUrl(shortCode) : null);
  const guestList = $derived(app.guests);
  const guests = $derived(guestList.length);
  const guestBusy = $derived(app.role === "guest" || app.starting === "guest");

  /** Screen state: starting, preparing, open room, busy with a guest session. */
  const stage = $derived(
    guestBusy ? "busy" : host ? "open" : status || app.starting === "host" ? "prep" : "idle",
  );

  const STEP: Partial<Record<Phase, [Key, Key]>> = {
    preparing: ["host.stepPreparing", "host.stepPreparingSub"],
    connecting: ["host.stepConnecting", "host.stepConnectingSub"],
    stopping: ["host.stepStopping", "host.stepStoppingSub"],
  };
  const step = $derived.by((): [string, string] => {
    if (!status) return [t("host.stepStarting"), t("host.stepStartingSub")];
    if (status.phase === "preparing" && app.world?.kind === "searching")
      return [t("host.stepSearching"), t("host.stepSearchingSub", { s: app.world.timeoutS })];
    const [title, sub] = STEP[status.phase] ?? STEP.stopping!;
    return [t(title), t(sub)];
  });

  /** Localized guest count ("1 guest", "5 guests" in en; Russian plural forms in ru). */
  const guestsWord = (n: number) => t("count.guests", { n });

  function start() {
    if (settings.portError) return;
    void app.startHost(settings.hostRequest());
  }

  // ---------------------------------------------------------------- worlds on this PC
  // The list is re-read every 5 s while there is no room: the player opens a world in the game, and
  // the "World" card immediately shows its name and icon.
  let worlds = $state<RecentWorld[] | null>(null);

  async function loadWorlds() {
    try {
      worlds = await core.recentWorlds(4);
    } catch {
      worlds = [];
    }
  }

  onMount(() => {
    void mods.refresh();
    void loadWorlds();
    const timer = setInterval(() => {
      if (stage === "idle" && !document.hidden) void loadWorlds();
    }, 5000);
    return () => clearInterval(timer);
  });

  /** The world for the card: the one open in the game, otherwise the last one. */
  const world = $derived(worlds?.[0] ?? null);

  // ---------------------------------------------------------------- mods
  /** Mods summary: a dot and detail (tooltip and screen reader text). */
  const modsInfo = $derived.by((): { dot: string; text: string; hint: string } => {
    const live = host?.mods;
    if (live?.error) return { dot: "dot-warn", text: t("host.modsNotShared"), hint: live.error };
    if (live) {
      if (!live.sharing) return { dot: "", text: t("host.modsSharingOff"), hint: t("host.modsNotNeeded") };
      if (live.files === 0) return { dot: "", text: t("host.noMods"), hint: t("host.modsFolderEmpty") };
      return {
        dot: "dot-live",
        text:
          live.sent_files > 0
            ? t("host.modsSent", { sent: live.sent_files, n: live.files, size: formatSize(live.sent_bytes) })
            : `${modsWord(live.files)} · ${formatSize(live.total_size)}`,
        hint: t("host.modsGuestHint"),
      };
    }
    if (!mods.overview) return { dot: "dot-busy dot-pulse", text: t("host.modsReading"), hint: t("host.modsLooking") };
    const ready = mods.shared;
    if (ready.files === 0) return { dot: "", text: t("host.noMods"), hint: t("host.modsNoJars") };
    return {
      dot: "",
      text: settings.modsShare ? `${modsWord(ready.files)} · ${formatSize(ready.size)}` : t("host.modsSharingOff"),
      hint: settings.modsShare ? t("host.modsWillStart") : t("host.modsNotNeeded"),
    };
  });

  // ---------------------------------------------------------------- stopping
  let confirming = $state(false);

  function askStop() {
    // There are guests in the room — stopping will disconnect them, so a question comes first.
    if (guests > 0) confirming = true;
    else void app.stop();
  }

  function confirmStop() {
    confirming = false;
    void app.stop();
  }

  // ---------------------------------------------------------------- world in the room and voice
  /** Joining without an account check is on, the world is found, but Phase Companion is not in it.
   *  A vanilla LAN world checks accounts, so official Minecraft on the host may block offline-mode
   *  friends. Some launchers (TLauncher) patch the check out themselves, and the game's process does not
   *  say so reliably: hence a calm note under the world line, not an alarm. */
  const accountCheckIdle = $derived(
    !!host?.look.world && !host.look.companion && !host.look.settings.account_check,
  );

  const roomWorld = $derived.by((): { warn: boolean; title: string; text: string } => {
    const world = app.role === "host" ? app.world : null;
    if (host && world?.kind === "checked" && world.error) {
      return { warn: true, title: t("host.portSilent", { port: world.port }), text: t("host.portSilentSub") };
    }
    if (host && world?.kind === "elsewhere") {
      return {
        warn: true,
        title: t("host.worldElsewhere", { port: world.port }),
        text: t("host.worldElsewhereSub", { current: world.current }),
      };
    }
    if (host) {
      return {
        warn: false,
        title: host.motd || (host.game_port ? t("host.portN", { port: host.game_port }) : t("host.noGame")),
        text: host.game_port
          ? [host.game_version ? `Minecraft ${host.game_version}` : null, t("host.portLower", { port: host.game_port })]
              .filter(Boolean)
              .join(" · ")
          : t("host.noPortSet"),
      };
    }
    return { warn: false, title: "", text: "" };
  });

  const voice = $derived.by((): { title: string; text: string } => {
    if (!host) {
      return { title: t("host.voiceAuto"), text: t("host.voiceAutoSub") };
    }
    if (host.voice.mode === "off") return { title: t("common.offShort"), text: t("journal.host.voiceOff") };
    if (host.voice.ports.length === 0)
      return { title: t("host.voiceNoPorts"), text: host.voice.hints[0] ?? t("host.voiceNoPortsSub") };
    return {
      title: host.voice.ports.map((p) => p.port).join(" · "),
      // Fallback ports have the same description — show each once.
      text: [...new Set(host.voice.ports.map((p) => p.description))].join("; "),
    };
  });

  const STEPS = $derived([t("host.help1"), t("host.help2"), t("host.help3")]);

  /** Who is in the room: "GrayIsko", "GrayIsko and Steve", "GrayIsko and 2 more" (localized). */
  const whoLine = $derived.by(() => {
    if (guests === 0) return useCode ? t("host.waitingCode") : t("host.waitingInvite");
    const names = guestList.map((g) => g.nickname ?? t("host.guestNoNick"));
    if (guests === 1) return t("host.whoOne", { a: names[0] });
    if (guests === 2) return t("host.whoTwo", { a: names[0], b: names[1] });
    return t("host.whoMore", { a: names[0], n: guests - 1 });
  });
  const slots = $derived(
    host && host.room.max_players > 0
      ? t("host.slotsOf", { used: guests + 1, max: host.room.max_players })
      : guestsWord(guests),
  );
</script>

<!-- How to find the world: automatically or by the port from the game chat. `uid` — an own field id in each popover. -->
{#snippet portSettings(uid: string)}
  <div class="pop-head">
    <div class="min-w-0">
      <p class="text-[15px] leading-tight font-semibold text-fg">{t("host.findWorld")}</p>
      <p class="mt-1 text-[12px] text-fg-2">{t("host.findWorldSub")}</p>
    </div>
  </div>
  <fieldset class="px-[18px] pb-4" disabled={app.starting !== null}>
    <legend class="sr-only">{t("host.worldPort")}</legend>
    <!-- No bind:group: inside a snippet Svelte 5 crashes on it at startup (binding_group). -->
    <div class="seg-group w-full">
      <label class="seg grow justify-center">
        <input
          type="radio"
          class="sr-only"
          name="{uid}-mode"
          checked={settings.portMode === "auto"}
          onchange={() => (settings.portMode = "auto")}
        />
        {t("host.portAuto")}
      </label>
      <label class="seg grow justify-center">
        <input
          type="radio"
          class="sr-only"
          name="{uid}-mode"
          checked={settings.portMode === "manual"}
          onchange={() => (settings.portMode = "manual")}
        />
        {t("host.portManual")}
      </label>
    </div>
    {#if settings.portMode === "manual"}
      <label class="label mt-3" for={uid}>{t("host.portFromChat")}</label>
      <input
        id={uid}
        class="field h-10 font-mono"
        inputmode="numeric"
        autocomplete="off"
        spellcheck="false"
        maxlength="5"
        placeholder="25565"
        bind:value={settings.port}
        aria-invalid={settings.portError !== null}
      />
      <p class="mt-2 text-[12px] leading-relaxed text-fg-2">
        {t("host.portChatHint")}
      </p>
      {#if settings.portError}<p class="error-text" role="alert">{settings.portError}</p>{/if}
    {:else}
      <p class="mt-3 text-[12px] leading-relaxed text-fg-2">
        {t("host.autoHint")}
      </p>
    {/if}
  </fieldset>
{/snippet}

<!-- The "Styling" card: the same before the room and in the room. -->
{#snippet lookCard()}
  <div class="room-row">
    <span class="room-row-icon"><Palette size={18} aria-hidden="true" /></span>
    <button type="button" class="room-row-link" onclick={() => nav.go("look")} data-look-open>
      <span class="room-row-title">{t("host.lookCard")}</span>
    </button>
    <span class="room-row-sub"><span class="room-row-hint">{t("host.lookCardSub")}</span></span>
  </div>
{/snippet}

<!-- Connection line: dashed while there is no room; solid once friends are inside. -->
{#snippet linkHead()}
  <div class="link-head" role="listitem" data-open={host ? "" : undefined} data-live={guests > 0 ? "" : undefined}>
    <span class="link-node link-self">
      <svg width="20" height="20" viewBox="0 0 26 26" aria-hidden="true">
        <path d="M12 2.5A9 9 0 0 0 12 20.5Z" fill="currentColor" />
        <path d="M14 5.5A9 9 0 0 1 14 23.5Z" fill="currentColor" opacity="0.55" />
      </svg>
    </span>
    <span class="link-wire" aria-hidden="true"></span>
    <span class="link-node tabular-nums">
      {#if guests > 0}{guests}{:else}<Users size={17} aria-hidden="true" />{/if}
    </span>
    <span class="link-label">{t("host.linkYou")}</span>
    <span class="link-label link-label-end">{t("host.linkFriends")}</span>
  </div>
{/snippet}

{#snippet modsCard()}
  <div class="room-row" data-host-mods>
    <span class="room-row-icon"><Package size={18} aria-hidden="true" /></span>
    {#if !host}
      <span class="room-row-corner">
        <button
          type="button"
          role="switch"
          class="card-switch"
          aria-checked={settings.modsShare}
          aria-label={t("host.shareMods")}
          onclick={() => (settings.modsShare = !settings.modsShare)}
        >
          <span class="mini-switch" aria-hidden="true"></span>
        </button>
      </span>
    {/if}
    <button type="button" class="room-row-link" onclick={() => nav.go("mods")}>
      <span class="room-row-title">{t("host.modsCard")}</span>
    </button>
    <span class="room-row-sub" title={modsInfo.hint}>
      {#if modsInfo.dot}<span class="dot {modsInfo.dot}" aria-hidden="true"></span>{/if}
      <span class="room-row-hint tabular-nums">{modsInfo.text}</span>
      <span class="sr-only">. {modsInfo.hint}</span>
    </span>
  </div>
{/snippet}

{#key stage}
  <div class="hero-copy fade-in" aria-live="polite">
    {#if stage === "idle"}
      <h1 id="hero-title" class="display">{t("host.hero1")}<br />{t("host.hero2")}</h1>
      <p class="lead">{t("host.lead")}</p>
      {#if profiles.error}
        <p class="note note-danger mt-5 max-w-[430px]" role="alert">
          <span>{t("host.keyUnread")}</span>
        </p>
      {/if}
      <div class="hero-actions">
        <button
          type="button"
          class="btn-pill btn-pill-fixed"
          onclick={start}
          disabled={app.starting !== null || settings.portError !== null}
          data-cta
        >
          {app.starting === "host" ? t("host.creating") : t("host.create")}
        </button>
        <HelpPopover id="host-help" steps={STEPS} {voice} />
      </div>
      <!-- If Phase doesn't see the world itself — the port from the game chat, in plain view, not only in the card. -->
      <button
        type="button"
        class="link mt-4 text-[13px]"
        popovertarget="port-pop"
        onclick={() => (settings.portMode = "manual")}
        data-port-link
      >
        {settings.portMode === "manual" && settings.port ? t("host.portChange", { port: settings.port }) : t("host.portSetManually")}
      </button>
      {#if settings.portError}
        <p class="error-text" role="alert">{settings.portError}</p>
      {/if}
    {:else if stage === "prep"}
      <h1 id="hero-title" class="display">{step[0]}.</h1>
      <p class="lead">{step[1]}</p>
      <div class="progress mt-9 w-full max-w-[420px]" role="progressbar" aria-label={t("host.preparingBar")}></div>
      <div class="hero-actions">
        <button type="button" class="btn-ghost-pill" onclick={() => app.stop()} disabled={app.stopping || !status}>
          <Square size={14} strokeWidth={2} aria-hidden="true" />
          {app.stopping ? t("host.cancelling") : t("common.cancel")}
        </button>
      </div>
    {:else if stage === "open" && host}
      <p class="status">
        <span class="dot dot-live dot-pulse" aria-hidden="true"></span>
        <span class="status-text tabular-nums" data-slots>{t("host.roomOpen")} · {slots}</span>
      </p>
      {#if useCode}
        <h1 id="hero-title" class="sr-only">{t("host.roomOpenCode")}</h1>
        {#if shortCode}
          <!-- The link sits right at the code: the code is for saying aloud, the link for a chat. -->
          <div class="room-code-row">
            <p class="room-code" data-room-code aria-label={t("host.roomCodeLabel", { code: shortCode.split("").join(" ") })}>{shortCode}</p>
            {#if inviteLink}
              <CopyButton text={inviteLink} label={t("host.copyLink")} title={inviteLink} glyph="link" class="code-link" icon={15} />
            {/if}
          </div>
        {:else}
          <p class="room-code" data-pending aria-live="polite">···-···</p>
        {/if}
        <p class="lead">
          {t("host.tellCode")}
          {#if host.relay.needs_token}{t("host.relayPassword")}{/if}
          {#if host.relay.needs_ca}{t("host.relayCert")}{/if}
        </p>
      {:else}
        <h1 id="hero-title" class="display">{t("host.openLine1")}<br />{t("host.openLine2")}</h1>
        <p class="lead">
          {t("host.sendInvite")}
          {#if host.relay.needs_token}{t("host.relayPasswordSeparate")}{/if}
          {#if host.relay.needs_ca}{t("host.relayCert")}{/if}
        </p>
        <!-- The invitation itself isn't shown on screen: the string is long and technical, a button copies it. -->
        {#if roomCode?.state === "unavailable"}
          <p class="mt-2 text-[13px] text-fg-2" role="status">{t("host.codeUnavailable")}</p>
        {/if}
      {/if}

      <div class="hero-actions" data-ticket={host.ticket}>
        {#if useCode}
          <CopyButton text={shortCode ?? ""} label={t("host.copyCode")} class="btn-pill" icon={17} orb disabled={!shortCode} />
        {:else}
          <CopyButton text={host.ticket} label={t("host.copyInvite")} class="btn-pill" icon={17} orb />
        {/if}
        <button type="button" class="btn-ghost-pill is-danger" onclick={askStop} disabled={app.stopping}>
          <Square size={13} strokeWidth={2} fill="currentColor" aria-hidden="true" />
          {app.stopping ? t("host.closing") : t("host.stop")}
        </button>
      </div>
      {#if useCode}
        <!-- Fallback if the friend's code service is unavailable: the long invitation is copied, not shown. -->
        <CopyButton text={host.ticket} label={t("host.copyLongInvite")} class="link mt-3 inline-flex items-center gap-1.5 text-[13px]" icon={13} />
      {/if}
    {:else}
      <h1 id="hero-title" class="display">{t("host.guestLine1")}<br />{t("host.guestLine2")}</h1>
      <p class="lead">{t("host.guestLead")}</p>
      <div class="hero-actions">
        <button type="button" class="btn-pill" onclick={() => nav.go("join")}>{t("host.toJoin")}</button>
      </div>
    {/if}
  </div>
{/key}

{#if stage === "idle"}
  <div class="room-panel fade-in" role="list" aria-label={t("host.beforeRoom")}>
    {@render linkHead()}
    <div class="room-row" role="listitem" data-active={world?.open ? "" : undefined} data-world-card>
      <span class="room-row-icon"><Globe size={18} aria-hidden="true" /></span>
      {#if world?.icon}
        <span class="room-row-corner"><img class="world-icon" src={world.icon} alt="" /></span>
      {/if}
      <button type="button" class="room-row-link" popovertarget="world-pop" aria-label={t("host.worldLabel", { name: world?.name ?? t("host.worldNotFound") })}>
        <span class="room-row-title truncate">{world?.name ?? t("host.yourWorld")}</span>
      </button>
      <span class="room-row-sub">
        {#if world?.open}
          <span class="dot dot-live" aria-hidden="true"></span><span class="room-row-hint">{t("host.openInGame")}</span>
        {:else if world}
          <span class="room-row-hint">{t("host.openToLan")}</span>
        {:else if worlds}
          <span class="room-row-hint">{t("host.openWorld")}</span>
        {:else}
          <span class="dot dot-busy dot-pulse" aria-hidden="true"></span><span class="room-row-hint">{t("host.searchingWorlds")}</span>
        {/if}
        {#if settings.portMode === "manual" && settings.port}
          <span class="shrink-0 font-mono text-[12px]">· {settings.port}</span>
        {/if}
      </span>
    </div>
    <div role="listitem" class="contents">{@render modsCard()}</div>
    <div role="listitem" class="contents">{@render lookCard()}</div>
  </div>

  <!-- How to find the world: automatically by the game's ports or by a given port. Opens from the
       "World" card and from the "Specify port manually" link — each button has its own popover below it. -->
  <Popover id="world-pop" label={t("host.findWorld")} align="start" width={340} height={250}>
    {@render portSettings("port")}
  </Popover>
  <Popover id="port-pop" label={t("host.findWorld")} align="start" width={340} height={250}>
    {@render portSettings("port-link")}
  </Popover>
{:else if stage === "open" && host}
  <div class="room-panel fade-in" role="list" aria-label={t("host.roomList")}>
    {@render linkHead()}
    <div class="room-row" role="listitem" data-active={guests > 0 ? "" : undefined} data-guests-line>
      <span class="room-row-icon"><Users size={18} aria-hidden="true" /></span>
      <span class="room-row-corner">
        {#if guests > 0}
          <span class="faces">
            {#each guestList.slice(0, 4) as g (g.id)}
              <span class="rounded-full ring-2 ring-black/40"><Avatar name={g.nickname} seed={g.id} size="sm" /></span>
            {/each}
          </span>
        {:else}
          <span class="grid size-7 place-items-center rounded-full border border-dashed border-white/40 text-white/80">
            <Users size={14} strokeWidth={2} aria-hidden="true" />
          </span>
        {/if}
      </span>
      <span class="room-row-title tabular-nums">{t("host.players")} · {slots}</span>
      <span class="room-row-sub">
        <span class="room-row-hint">{whoLine}</span>
        <button type="button" class="link relative z-[2] shrink-0 text-[13px]" onclick={() => nav.go("look")} data-slots-link>
          {t("host.slotsLink")}
        </button>
      </span>
    </div>
    <div class="room-row" role="listitem">
      <span class="room-row-icon"><Globe size={18} aria-hidden="true" /></span>
      <span class="room-row-title truncate">{roomWorld.title}</span>
      <span class="room-row-sub" role={roomWorld.warn ? "status" : undefined}>
        <span class="dot {roomWorld.warn ? 'dot-warn' : 'dot-live'}" aria-hidden="true"></span>
        <span class="room-row-hint">{roomWorld.text}</span>
      </span>
      {#if accountCheckIdle}
        <span class="room-row-note" data-account-idle>
          <span class="room-row-hint">{t("host.accountCheckIdle")}</span>
          <button type="button" class="link relative z-[2] shrink-0 text-[13px]" onclick={() => nav.go("look")} data-account-idle-link>
            {t("host.accountCheckIdleLink")}
          </button>
        </span>
      {/if}
    </div>
    <div role="listitem" class="contents">{@render modsCard()}</div>
    <div role="listitem" class="contents">{@render lookCard()}</div>
  </div>
{/if}


<Modal
  open={confirming}
  label={t("host.inRoom", { guests: guestsWord(guests) })}
  title={t("host.stopTitle")}
  onclose={() => (confirming = false)}
>
  <p>{t("host.stopBody")}</p>
  {#snippet actions()}
    <button type="button" class="btn btn-lg" onclick={() => (confirming = false)}>
      {t("host.keep")} <span class="kbd">Esc</span>
    </button>
    <button type="button" class="btn btn-danger-solid btn-lg" onclick={confirmStop}>{t("host.stop")}</button>
  {/snippet}
</Modal>
