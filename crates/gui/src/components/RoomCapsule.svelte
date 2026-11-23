<script lang="ts">
  // The live "Room and guests" capsule in the top row. The capsule holds a state dot, a word
  // and a counter: for a host — how many guests, for a guest — ping to the host, without a
  // session — "0". A click opens a popover with details: state and world, guests (large
  // nickname, short ID, route and ping), for a guest — the host, game and voice ports, network,
  // and the list of the profile's allowed devices (they are let in without the "Let them in?"
  // question).
  import Check from "@lucide/svelte/icons/check";
  import Copy from "@lucide/svelte/icons/copy";
  import Route from "@lucide/svelte/icons/route";
  import ShieldCheck from "@lucide/svelte/icons/shield-check";
  import UserMinus from "@lucide/svelte/icons/user-minus";
  import Users from "@lucide/svelte/icons/users";
  import { app } from "../lib/app.svelte";
  import { copyText } from "../lib/clipboard";
  import { shortId, type PathKind, type Phase, type Role } from "../lib/core";
  import { t, type Key } from "../lib/i18n/index.svelte";
  import { nav } from "../lib/nav.svelte";
  import { profiles } from "../lib/profiles.svelte";
  import { settings } from "../lib/settings.svelte";
  import Avatar from "./Avatar.svelte";
  import Popover from "./Popover.svelte";

  const ROUTE: Record<PathKind, Key> = { direct: "room.routeDirect", relay: "room.routeRelay", other: "room.routeOther" };
  const PHASE: Record<Role, Partial<Record<Phase, Key>>> = {
    host: {
      preparing: "room.hostPreparing",
      connecting: "room.hostConnecting",
      running: "room.hostRunning",
      stopping: "room.hostStopping",
    },
    guest: {
      preparing: "room.guestPreparing",
      connecting: "room.guestConnecting",
      running: "room.guestRunning",
      reconnecting: "room.guestReconnecting",
      stopping: "room.guestStopping",
    },
  };

  const status = $derived(app.active ? app.status : null);
  const host = $derived(status?.host ?? null);
  const guest = $derived(status?.guest ?? null);
  const guests = $derived(app.guests);
  const live = $derived(status?.phase === "running");
  const lost = $derived(status?.phase === "reconnecting");
  const hostLink = $derived(app.hostLink);
  const relay = $derived(host ?? guest);
  /** Room slots (for a guest — what the host reported). */
  const room = $derived(host?.room ?? guest?.room ?? null);

  /** "Room slots" lives in the "Room styling" window. */
  function showSlots() {
    document.getElementById("room-pop")?.hidePopover();
    nav.go("look");
  }
  /** Nickname of the running session (friends already have it), otherwise the profile's nickname. */
  const nickname = $derived(status?.nickname ?? profiles.nickname);
  const ms = (rtt: number | null) => (rtt === null ? "—" : t("room.ms", { n: Math.round(rtt) }));
  /** Session state word in the current language (`null` — no word for this phase). */
  const phaseText = (role: Role, phase: Phase): string | null => {
    const key = PHASE[role][phase];
    return key ? t(key) : null;
  };

  /** What the capsule itself shows. */
  const view = $derived.by((): { dot: string; label: string; count: string; host: boolean } => {
    if (!status) {
      return { dot: profiles.error ? "dot-danger" : "dot-live", label: t("room.title"), count: "0", host: true };
    }
    if (status.role === "host") {
      return {
        dot: live ? "dot-live dot-pulse" : "dot-busy dot-pulse",
        label: live ? t("room.title") : (phaseText("host", status.phase) ?? t("room.title")),
        count: String(guests.length),
        host: true,
      };
    }
    return {
      dot: lost ? "dot-warn dot-pulse" : live ? "dot-live dot-pulse" : "dot-busy dot-pulse",
      label: lost ? t("room.lost") : live ? t("room.guesting") : t("room.connecting"),
      count: ms(hostLink?.rttMs ?? null),
      host: false,
    };
  });

  const capsuleLabel = $derived(
    !status
      ? t("room.capsuleIdle")
      : status.role === "host"
        ? t("room.capsuleHost", { state: phaseText("host", status.phase) ?? status.phase, n: guests.length })
        : t("room.capsuleGuest", { state: phaseText("guest", status.phase) ?? status.phase, ping: ms(hostLink?.rttMs ?? null) }),
  );

  let open = $state(false);

  // The host allowed a new device while the popup is open — the list is re-read immediately.
  $effect(() => {
    void app.allowedAdded;
    void settings.profile;
    if (open) void profiles.loadAllowed();
  });

  const allowed = $derived(profiles.allowed ?? []);
  /** Friends who are in the host's room right now. */
  const inRoom = $derived(new Set(status?.role === "host" ? guests.map((g) => g.id) : []));

  // Friend row actions: copy the full ID, remove from quick join (after a confirmation).
  let confirming = $state<string | null>(null);
  let busy = $state(false);
  let forgetError = $state<string | null>(null);
  let copied = $state<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;

  async function copyId(id: string) {
    if (!(await copyText(id))) return;
    copied = id;
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied = null), 1600);
  }

  async function forget(id: string) {
    busy = true;
    forgetError = null;
    try {
      await profiles.forget(id);
      confirming = null;
    } catch (e) {
      forgetError = String(e);
    } finally {
      busy = false;
    }
  }

  // The popover closed: a pending remove confirmation must not wait for the next opening.
  $effect(() => {
    if (!open) {
      confirming = null;
      forgetError = null;
    }
  });
</script>

{#snippet route(path: PathKind | null)}
  <span class="badge {path === 'direct' ? 'badge-green' : path ? 'badge-blue' : ''}">{path ? t(ROUTE[path]) : "—"}</span>
{/snippet}

{#snippet ping(rtt: number | null, path: PathKind | null)}
  <span class="badge badge-mono {rtt === null ? '' : path === 'direct' ? 'badge-green' : 'badge-blue'}">{ms(rtt)}</span>
{/snippet}

<div class="capsule">
  <button
    type="button"
    class="capsule-btn"
    popovertarget="room-pop"
    aria-expanded={open}
    aria-label={capsuleLabel}
    data-room-capsule
  >
    <span class="dot {view.dot}" aria-hidden="true"></span>
    <span class="hide-compact" aria-hidden="true">{view.label}</span>
    <span class="capsule-count" aria-hidden="true">
      {#if view.host}<Users size={12} strokeWidth={2.2} />{/if}{view.count}
    </span>
  </button>
</div>

<Popover id="room-pop" label={t("room.popover")} width={372} height={420} bind:open>
  <!-- State: what kind of session it is and how it's doing. -->
  <div class="pop-head" aria-live="polite">
    <div class="min-w-0">
      <p class="text-[16px] leading-tight font-semibold text-fg">
        {status ? (status.role === "host" ? t("room.title") : t("room.linkTitle")) : t("room.title")}
      </p>
      <p class="mt-1.5 flex items-center gap-2 text-[13px] text-fg-2">
        <span class="dot {view.dot}" aria-hidden="true"></span>
        {#if status}
          {phaseText(status.role, status.phase) ?? status.phase}
        {:else}
          {profiles.error ? t("room.keyUnread") : t("room.idle")}
        {/if}
      </p>
    </div>
    <span class="badge max-w-[150px] min-w-0 shrink-0">
      <span class="shrink-0">{status ? (status.role === "host" ? t("room.roleHost") : t("room.roleGuest")) : t("room.roleYou")}</span>
      {#if nickname}<span class="shrink-0" aria-hidden="true">·</span><span class="truncate">{nickname}</span>{/if}
    </span>
  </div>

  {#if host || guest}
    <p class="-mt-1 flex min-w-0 items-center gap-2 px-[18px] pb-3 text-[13px] text-fg-2">
      <span class="truncate">{t("room.world")} <span class="font-medium text-fg">{t("room.worldName", { name: host?.motd ?? guest?.motd ?? "" })}</span></span>
    </p>
  {/if}

  {#if status?.role === "host"}
    <section class="pop-section" aria-labelledby="pop-guests">
      <h3 id="pop-guests" class="pop-label flex items-center justify-between gap-3">
        <span>{t("room.guests")} <span class="tabular-nums">({guests.length})</span></span>
        {#if room && room.max_players > 0}
          <span class="font-normal tabular-nums" data-room-slots>
            {t("room.slotsUsed", { used: guests.length + 1, n: room.max_players })}
          </span>
        {/if}
      </h3>
      {#if guests.length === 0}
        <div class="flex items-center gap-3 rounded-[12px] border border-dashed border-line-strong px-3 py-2.5">
          <span class="grid size-8 shrink-0 place-items-center rounded-full bg-elevated text-fg-2">
            <Users size={15} strokeWidth={2} aria-hidden="true" />
          </span>
          <p class="text-[13px] text-fg-2">
            {host ? t("room.noGuestsHost") : t("room.noGuestsWait")}
          </p>
        </div>
      {:else}
        <ul class="flex flex-col" style="--avatar-ring: var(--bg-panel)">
          {#each guests as g (g.id)}
            <li class="pop-row" data-guest={g.id}>
              <Avatar name={g.nickname} seed={g.id} size="md" dot="dot-live" />
              <div class="min-w-0 grow">
                <p class="truncate text-[14px] leading-tight font-semibold text-fg">
                  {g.nickname ?? t("common.noNickname")}
                </p>
                <p class="mt-1 truncate font-mono text-[11px] leading-none text-fg-3">
                  {shortId(g.id)}{#if g.connections > 1}<span class="font-sans"> · {t("room.connections", { n: g.connections })}</span>{/if}
                </p>
              </div>
              <div class="flex shrink-0 flex-col items-end gap-1">
                {@render route(g.path)}
                {@render ping(g.rttMs, g.path)}
              </div>
            </li>
          {/each}
        </ul>
      {/if}
      {#if host}
        <button type="button" class="link mt-2.5 text-[12px]" onclick={showSlots} data-slots-link>
          {t("room.slotsLink")}
        </button>
      {/if}
    </section>
  {:else if status?.role === "guest"}
    <section class="pop-section" aria-labelledby="pop-host">
      <h3 id="pop-host" class="pop-label">{t("room.roleHost")}</h3>
      <div class="pop-row" style="--avatar-ring: var(--bg-panel)">
        <Avatar name={guest?.host_nickname} seed={guest?.host_id} size="md" dot={guest ? (lost ? "dot-warn" : "dot-live") : undefined} />
        <div class="min-w-0 grow">
          <p class="truncate text-[14px] leading-tight font-semibold text-fg" data-host-nickname>
            {guest ? (guest.host_nickname ?? t("common.noNickname")) : t("room.waitingAnswer")}
          </p>
          <p class="mt-1 truncate font-mono text-[11px] leading-none text-fg-3">
            {guest ? shortId(guest.host_id) : "—"}
          </p>
        </div>
        <div class="flex shrink-0 flex-col items-end gap-1">
          {@render route(hostLink?.path ?? null)}
          {@render ping(hostLink?.rttMs ?? null, hostLink?.path ?? null)}
        </div>
      </div>
    </section>
  {/if}

  <!-- Ports and network (in a session) or device details (without a session). -->
  <section class="pop-section" aria-labelledby="pop-net">
    <h3 id="pop-net" class="pop-label flex items-center gap-1.5">
      <Route size={13} strokeWidth={2} aria-hidden="true" />{status ? t("room.portsRoute") : t("room.network")}
    </h3>
    <dl>
      {#if status?.role === "host"}
        <div class="row py-1">
          <dt>{t("room.gameTcp")}</dt>
          <dd class="font-mono">{host?.game_port ?? "—"}</dd>
        </div>
        <div class="row py-1">
          <dt>{t("room.voiceUdp")}</dt>
          <dd class="font-mono">
            {host ? (host.voice.ports.length > 0 ? host.voice.ports.map((p) => p.port).join(", ") : t("common.offShort")) : "—"}
          </dd>
        </div>
      {:else if status?.role === "guest"}
        <div class="row py-1">
          <dt>{t("room.gameTcp")}</dt>
          <dd class="font-mono">{guest?.local_addr ?? "—"}</dd>
        </div>
        <div class="row py-1">
          <dt>{t("room.voiceUdp")}</dt>
          <dd class="font-mono">
            {guest ? (guest.voice_ports.length > 0 ? guest.voice_ports.join(", ") : t("common.offShort")) : "—"}
          </dd>
        </div>
      {:else}
        <div class="row py-1">
          <dt>{t("room.nickname")}</dt>
          <dd class="truncate font-semibold">{nickname || "—"}</dd>
        </div>
        <div class="row py-1">
          <dt>{t("room.profile")}</dt>
          <dd class="font-mono">{settings.profile}</dd>
        </div>
        <div class="row py-1">
          <dt>ID</dt>
          <dd class="font-mono">{profiles.current?.device_id ? shortId(profiles.current.device_id) : "—"}</dd>
        </div>
      {/if}
      <div class="row py-1">
        <dt>{t("room.network")}</dt>
        <dd>
          {#if !relay}
            {settings.networkLabel}
          {:else if relay.relay.loopback_only}
            {t("network.local")}
          {:else if relay.relay.custom}
            {t("room.relayCustom")}
          {:else}
            {t("room.relayN0")}
          {/if}
        </dd>
      </div>
    </dl>
  </section>

  <!-- The profile's allowed devices: they are let in without a question. -->
  <section class="pop-section" aria-labelledby="pop-allowed">
    <h3 id="pop-allowed" class="pop-label flex items-center gap-1.5">
      <ShieldCheck size={13} strokeWidth={2} aria-hidden="true" />
      {t("room.friends")} <span class="tabular-nums">({profiles.allowed?.length ?? profiles.current?.allowed_devices ?? 0})</span>
    </h3>
    {#if profiles.allowedError}
      <p class="text-[12px] text-danger-text">{profiles.allowedError}</p>
    {:else if profiles.allowed === null}
      <div class="space-y-2" aria-hidden="true">
        {#each [64, 48] as w (w)}
          <div class="h-7 rounded-full bg-elevated" style:width="{w}%"></div>
        {/each}
      </div>
    {:else if allowed.length === 0}
      <p class="text-[13px] text-fg-2">
        {t("room.noFriends")}
      </p>
    {:else}
      <!-- List rows stick out 8px past the edge (hover highlight) — the scroll area
           has the same margin on the sides, otherwise a horizontal bar would appear. -->
      <ul class="-mx-2 flex max-h-[168px] flex-col overflow-x-hidden overflow-y-auto px-2" data-allowed>
        {#each allowed as d (d.device_id)}
          <li class="pop-row friend-row py-1.5" data-friend={d.device_id}>
            <Avatar name={d.nickname} seed={d.device_id} size="xs" />
            <span class="flex min-w-0 grow items-center gap-1.5">
              <span class="truncate text-[13px] font-medium text-fg">{d.nickname ?? t("common.noNickname")}</span>
              {#if inRoom.has(d.device_id)}<span class="badge badge-green shrink-0">{t("room.friendInRoom")}</span>{/if}
            </span>
            {#if confirming === d.device_id}
              <span class="shrink-0 text-[12px] text-fg-2">{t("room.forgetAsk")}</span>
              <button type="button" class="friend-btn is-danger" onclick={() => forget(d.device_id)} disabled={busy} data-forget-yes>
                {t("room.forgetYes")}
              </button>
              <button type="button" class="friend-btn" onclick={() => (confirming = null)} disabled={busy}>{t("room.forgetNo")}</button>
            {:else}
              <span class="shrink-0 font-mono text-[11px] text-fg-3">{shortId(d.device_id)}</span>
              <button
                type="button"
                class="friend-icon"
                onclick={() => copyId(d.device_id)}
                title={copied === d.device_id ? t("room.idCopied") : t("room.copyId")}
                aria-label={copied === d.device_id ? t("room.idCopied") : t("room.copyId")}
              >
                {#if copied === d.device_id}<Check size={14} strokeWidth={2.4} aria-hidden="true" />{:else}<Copy size={14} strokeWidth={2} aria-hidden="true" />{/if}
              </button>
              <button
                type="button"
                class="friend-icon is-danger"
                onclick={() => (confirming = d.device_id)}
                title={t("room.forget")}
                aria-label={t("room.forget")}
                data-forget
              >
                <UserMinus size={14} strokeWidth={2} aria-hidden="true" />
              </button>
            {/if}
          </li>
        {/each}
      </ul>
      {#if forgetError}<p class="mt-2 text-[12px] text-danger-text" role="alert">{forgetError}</p>{/if}
    {/if}
  </section>
</Popover>
