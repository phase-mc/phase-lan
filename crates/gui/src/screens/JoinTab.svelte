<script lang="ts">
  // The "Join" tab (guest) sits on the world picture, like "Host".
  // Before a session: "Join a world.", a clean code field, the blue "Join" button
  // and "How do I connect the world?" (three steps and voice). Until the host
  // answers — what nickname the guest is knocking under and their ID in groups for checking. After
  // connecting — whose guest you are, ping, route and voice, the address for Minecraft with
  // copying, and the room's mods card (when mods are required).
  import LogOut from "@lucide/svelte/icons/log-out";
  import Ticket from "@lucide/svelte/icons/ticket";
  import X from "@lucide/svelte/icons/x";
  import Avatar from "../components/Avatar.svelte";
  import CopyButton from "../components/CopyButton.svelte";
  import DeviceId from "../components/DeviceId.svelte";
  import HelpPopover from "../components/HelpPopover.svelte";
  import ModSyncCard from "../components/ModSyncCard.svelte";
  import { app } from "../lib/app.svelte";
  import { tick } from "svelte";
  import { isRoomCode, NICKNAME_MAX, shortId, type PathKind } from "../lib/core";
  import { t, type Key } from "../lib/i18n/index.svelte";
  import { codeFromInput } from "../lib/invite";
  import { look } from "../lib/look.svelte";
  import { nav } from "../lib/nav.svelte";
  import { cleanNickname, profiles } from "../lib/profiles.svelte";
  import { settings } from "../lib/settings.svelte";

  const status = $derived(app.role === "guest" ? app.status : null);
  const guest = $derived(status?.guest ?? null);
  const link = $derived(app.hostLink);
  /** Nickname of the running session (the host already sees it), otherwise the profile's nickname. */
  const nickname = $derived(status?.nickname ?? profiles.nickname);
  const ping = $derived(link?.rttMs ?? null);
  const hostBusy = $derived(app.role === "host" || app.starting === "host");

  const stage = $derived(
    hostBusy ? "busy" : guest ? "live" : status || app.starting === "guest" ? "wait" : "idle",
  );

  const lost = $derived(status?.phase === "reconnecting");

  const ticket = $derived(app.draftTicket.trim());
  /** Room code `K7F-2QX` (in any form: lowercase, without a hyphen, or inside an invite link
   *  from the site) — Phase finds the invitation on the code service itself. A named code
   *  (`vanilla-crew`) doesn't work yet. */
  const roomCode = $derived(codeFromInput(ticket));
  /** The code came with an invite link: the line under the field says so. */
  const fromLink = $derived(roomCode !== null && roomCode === app.linkCode);
  const vanityCode = $derived(!roomCode && isRoomCode(ticket) ? ticket : null);
  const ticketError = $derived(
    ticket && !roomCode && !vanityCode && !ticket.startsWith("endpoint")
      ? t("join.badCode")
      : null,
  );
  const canJoin = $derived(ticket !== "" && ticketError === null && vanityCode === null);

  // ---------------------------------------------------------------- nickname in place
  // "Change" turns the name into a field: Enter and losing focus save, Esc cancels.
  let nickEditing = $state(false);
  let nickDraft = $state("");
  let nickError = $state<string | null>(null);
  let nickInput = $state<HTMLInputElement>();
  let nickSaving = false;

  async function editNick() {
    nickDraft = nickname;
    nickError = null;
    nickEditing = true;
    await tick();
    nickInput?.focus();
    nickInput?.select();
  }

  async function saveNick() {
    if (!nickEditing || nickSaving) return;
    if (cleanNickname(nickDraft) === nickname) {
      nickEditing = false;
      return;
    }
    nickSaving = true;
    const error = await profiles.setNickname(nickDraft);
    nickSaving = false;
    nickError = error;
    // An error (empty nickname, too long) — we stay in the field; the plate turns red.
    if (!error) nickEditing = false;
  }

  /** Focus left: an empty nickname — cancel, anything else is saved. */
  function blurNick() {
    if (cleanNickname(nickDraft) === "") {
      nickError = null;
      nickEditing = false;
    } else void saveNick();
  }

  function nickKey(event: KeyboardEvent) {
    if (event.key === "Enter") {
      event.preventDefault();
      void saveNick();
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      nickError = null;
      nickEditing = false;
    }
  }

  function join(event: SubmitEvent) {
    event.preventDefault();
    if (!canJoin || app.starting) return;
    void app.join({
      profile: settings.profile,
      // A pasted invite link goes to the core as the bare code.
      ticket: roomCode ?? ticket,
      net: settings.net(),
      mods: settings.guestMods(),
    });
  }

  const ROUTE: Record<PathKind, Key> = { direct: "room.routeDirect", relay: "room.routeRelay", other: "room.routeOther" };
  /** The room's mods card — only when mods require something from the guest. */
  const showMods = $derived(app.modSync !== null && app.modSync.state !== "not_required");

  const JOIN_STEPS = $derived([t("join.help1"), t("join.help2"), t("join.help3")]);
  const JOIN_VOICE = $derived({ title: t("join.voiceAuto"), text: t("join.voiceAutoSub") });

  /** "How to join the world" — steps once in the room. */
  const enterSteps = $derived.by(() => {
    if (!guest) return JOIN_STEPS;
    return guest.lan_name
      ? [
          t("join.enter1"),
          t("join.enter2", { name: guest.lan_name }),
          t("join.enter3", { addr: guest.local_addr }),
        ]
      : [t("join.enter1"), t("join.enter3b", { addr: guest.local_addr })];
  });
  const liveVoice = $derived(
    guest && guest.voice_ports.length > 0
      ? { title: guest.voice_ports.join(" · "), text: t("join.voiceOpen") }
      : { title: t("common.offShort"), text: t("join.voiceNone") },
  );
</script>

{#snippet netStatus(badge: string, dot: string, text: string)}
  <p class="status">
    <span class="status-badge"><span class="dot {dot}" aria-hidden="true"></span>{badge}</span>
    <span class="status-text">{text}</span>
  </p>
{/snippet}

{#key stage}
      <div class="hero-copy fade-in">
        {#if stage === "idle"}
          <form class="contents" onsubmit={join}>
            <h1 id="join-title" class="display">{t("join.hero1")}<br />{t("join.hero2")}</h1>
            <p class="lead">
              {t("join.lead")}
            </p>

            <div class="mt-7 w-full max-w-[520px]">
              <label class="label" for="ticket">{t("join.fieldLabel")}</label>
              <div class="relative">
                <Ticket
                  size={18}
                  strokeWidth={2}
                  class="pointer-events-none absolute top-1/2 left-4 -translate-y-1/2 text-fg-3"
                  aria-hidden="true"
                />
                <!-- svelte-ignore a11y_autofocus -->
                <input
                  id="ticket"
                  class="field field-hero"
                  spellcheck="false"
                  autocomplete="off"
                  autofocus
                  placeholder={t("join.placeholder")}
                  bind:value={app.draftTicket}
                  disabled={app.starting !== null}
                  aria-invalid={ticketError !== null}
                  aria-describedby={ticketError ? "ticket-error" : roomCode ? "ticket-code" : "ticket-hint"}
                />
              </div>
              {#if ticketError}
                <p id="ticket-error" class="error-text" role="alert">{ticketError}</p>
              {:else if roomCode}
                <p id="ticket-code" class="mt-3 text-[13px] text-fg-2" role="status" data-code-line={fromLink ? "link" : "code"}>
                  {t("join.codeFound")} <span class="font-mono text-fg">{roomCode}</span>
                  {fromLink ? t("join.codeFromLink") : t("join.codeFound2")}
                </p>
              {:else if vanityCode}
                <p id="ticket-code" class="mt-3 text-[13px] text-fg-2" role="status">
                  {t("join.vanity")}
                </p>
              {/if}
              <div id="ticket-hint" class="nick-plate mt-2.5" data-invalid={nickError !== null} data-nick-plate>
                <span class="shrink-0">{t("join.yourNick")}</span>
                {#if nickEditing}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input
                    bind:this={nickInput}
                    bind:value={nickDraft}
                    class="min-w-0 grow bg-transparent text-[13px] font-semibold text-fg outline-none"
                    maxlength={NICKNAME_MAX}
                    spellcheck="false"
                    autocomplete="off"
                    autofocus
                    aria-label={t("join.yourNickLabel")}
                    aria-invalid={nickError !== null}
                    onkeydown={nickKey}
                    onblur={blurNick}
                  />
                  <span class="shrink-0 pr-2 font-mono text-[11px] text-fg-3 tabular-nums" aria-hidden="true">
                    {[...cleanNickname(nickDraft)].length}/{NICKNAME_MAX}
                  </span>
                {:else}
                  <span class="min-w-0 grow truncate font-semibold text-fg" data-nickname>{nickname || "…"}</span>
                  <button
                    type="button"
                    class="pill-action h-[30px]"
                    onclick={editNick}
                    disabled={!nickname || app.starting !== null}
                  >
                    {t("common.edit")}
                  </button>
                {/if}
              </div>
              {#if nickError}<p class="sr-only" role="alert">{nickError}</p>{/if}
            </div>

            <div class="hero-actions">
              <button
                type="submit"
                class="btn-pill btn-pill-fixed"
                disabled={app.starting !== null || !canJoin}
                data-cta
              >
                {app.starting === "guest" ? t("join.joining") : t("nav.join")}
              </button>
              <HelpPopover id="join-help" steps={JOIN_STEPS} voice={JOIN_VOICE} />
            </div>
          </form>
        {:else if stage === "busy"}
          <h1 id="join-title" class="display">{t("join.hostLine1")}<br />{t("join.hostLine2")}</h1>
          <p class="lead">{t("join.hostLead")}</p>
          <div class="hero-actions">
            <button type="button" class="btn-pill" onclick={() => nav.go("host")}>{t("join.toRoom")}</button>
          </div>
        {:else if stage === "wait"}
          <div class="flex w-full flex-col items-start" aria-live="polite">
            <h1 id="join-title" class="display">{t("join.waitLine1")}<br />{t("join.waitLine2")}</h1>
            <p class="lead">
              {t("join.waitLead")}
            </p>
            <div class="progress mt-7 max-w-[420px]" role="progressbar" aria-label={t("join.connecting")}></div>

            <!-- What the host sees in the "Let them in?" question: this device's nickname and ID. -->
            <section class="mt-7 w-full max-w-[540px]" aria-labelledby="knock-title">
              <h2 id="knock-title" class="sr-only">{t("join.hostSeesYou")}</h2>
              <div class="player-card flex items-center gap-4 p-4">
                <Avatar name={nickname || null} seed={status?.device_id} size="lg" />
                <div class="min-w-0 grow">
                  <p class="text-[13px] text-fg-2">{t("join.knockingAs")}</p>
                  <p class="truncate text-[22px] leading-tight font-semibold text-fg" data-nickname>
                    {nickname || "…"}
                  </p>
                </div>
                <span class="badge badge-blue shrink-0 self-start">{t("join.waitingBadge")}</span>
              </div>
              <p class="mt-4 mb-2 text-[12px] font-medium text-fg-2">
                {t("join.idGroups")}
              </p>
              {#if status?.device_id}
                <DeviceId id={status.device_id} compact />
              {:else}
                <p class="text-fg-2">{t("join.readingKey")}</p>
              {/if}
            </section>

            <div class="hero-actions">
              {#if status?.device_id}
                <CopyButton text={status.device_id} label={t("join.copyId")} class="btn-pill" icon={17} orb />
              {/if}
              <button type="button" class="btn-ghost-pill" onclick={() => app.stop()} disabled={app.stopping}>
                <X size={16} strokeWidth={2} aria-hidden="true" />
                {app.stopping ? t("host.cancelling") : t("common.cancel")}
              </button>
            </div>
          </div>
        {:else if stage === "live" && guest && status}
          {#if lost}
            {@render netStatus(t("room.lost"), "dot-warn dot-pulse", t("join.linkLost"))}
          {:else}
            {@render netStatus(t("join.online"), "dot-live dot-pulse", t("join.connectedTo", { motd: guest.motd }))}
          {/if}
          <h1 id="join-title" class="display">
            {#if lost}{t("join.lostLine1")}<br />{t("join.lostLine2")}{:else}{t("join.inWorld")}<br /><span data-host-nickname>{guest.host_nickname ?? t("join.friend")}</span>.{/if}
          </h1>
          <p class="lead">
            {#if lost}
              {t("join.reconnecting")}
            {:else if guest.lan_name}
              {t("join.liveLan", { name: guest.lan_name })}
            {:else}
              {t("join.liveDirect")}
            {/if}
          </p>

          <!-- Ping, route, voice — on one line; details are in the room capsule at the top. -->
          <p class="live-line" aria-live="polite" data-live-line>
            <span class="tabular-nums">{lost ? t("room.lost") : ping === null ? t("join.pingNone") : t("join.ping", { n: Math.round(ping) })}</span>
            <span aria-hidden="true">·</span>
            <span>{link?.path ? t(ROUTE[link.path]) : "—"}</span>
            <span aria-hidden="true">·</span>
            <span>{t("join.voice")} <span class="font-mono text-fg">{guest.voice_ports.length > 0 ? guest.voice_ports.join(" · ") : t("common.offShort")}</span></span>
          </p>

          <div class="block-gap w-full max-w-[540px]">
            <p class="mb-2 flex items-center justify-between gap-3 text-[12px] font-medium text-fg-2">
              <span>{t("join.address")}</span>
              <span class="truncate font-mono text-fg-3">
                {t("join.hostId", { nick: guest.host_nickname ?? "", id: shortId(guest.host_id) })}
              </span>
            </p>
            <p class="ticket font-mono text-[16px] leading-tight" data-address>{guest.local_addr}</p>
          </div>

          <div class="hero-actions">
            <!-- While the guest hasn't decided about mods, the screen's main button is "Sync". -->
            <CopyButton
              text={guest.local_addr}
              label={t("join.copyAddress")}
              class={app.modsWaiting ? "btn-ghost-pill" : "btn-pill"}
              icon={17}
              orb={!app.modsWaiting}
            />
            <button type="button" class="btn-ghost-pill is-danger" onclick={() => app.stop()} disabled={app.stopping}>
              <LogOut size={16} strokeWidth={2} aria-hidden="true" />
              {app.stopping ? t("join.disconnecting") : t("join.disconnect")}
            </button>
            <HelpPopover
              id="enter-help"
              label={t("join.enterHelp")}
              steps={enterSteps}
              voice={liveVoice}
              note={guest.game_version ? t("join.hostVersion", { version: guest.game_version }) : null}
              compact
            />
          </div>

          {#if look.mine?.enabled}
            {@const mine = look.mine}
            <!-- Access is decided by the host (`can_glow`): free while they allow guests highlight. -->
            <button
              type="button"
              role="switch"
              class="toggle-chip block-gap"
              aria-checked={mine.glow}
              disabled={!mine.can_glow}
              onclick={() => look.setOwnGlow(!mine.glow)}
              data-own-glow
            >
              {t("join.glowing")}
              <span class="mini-switch" aria-hidden="true"></span>
              <span class="w-8 text-left" aria-hidden="true">{mine.glow ? t("common.on") : t("common.off")}</span>
            </button>
            {#if look.error}
              <p class="mt-2 text-[12px] text-danger-text" role="alert">{look.error}</p>
            {/if}
          {/if}

          {#if showMods}
            <div class="block-gap w-full max-w-[560px]">
              <ModSyncCard context="join" />
            </div>
          {/if}
        {/if}
      </div>
{/key}
