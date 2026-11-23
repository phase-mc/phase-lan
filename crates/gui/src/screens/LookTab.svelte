<script lang="ts">
  // The "Styling" tab — the host's panel: what players see in Minecraft and how many there are.
  // On top — the styling state and the join header: "PHASE • code • online".
  // Below — players: a live preview "[TAG] name", the "Color and tag" button (a popover with the palette) and
  // the "Silhouette through walls" toggle; then "Room slots" with an honest load hint.
  // All free for any host (owner's decision, 2026-11-18).
  import Minus from "@lucide/svelte/icons/minus";
  import Plus from "@lucide/svelte/icons/plus";
  import { onDestroy } from "svelte";
  import {
    DEFAULT_ACCENT,
    findColor,
    roomCapacity,
    shortId,
    SLOTS,
    slotsLoad,
    TITLE_MAX,
    type LookMode,
    type LookPlayer,
    type SlotsLoad,
  } from "../lib/core";
  import { t, type Key } from "../lib/i18n/index.svelte";
  import { look } from "../lib/look.svelte";
  import { profiles } from "../lib/profiles.svelte";
  import AccountCheckToggle from "../components/AccountCheckToggle.svelte";
  import CompanionCard from "../components/CompanionCard.svelte";
  import Avatar from "../components/Avatar.svelte";
  import CopyButton from "../components/CopyButton.svelte";
  import GamePreview from "../components/GamePreview.svelte";
  import Popover from "../components/Popover.svelte";
  import StyleEditor from "../components/StyleEditor.svelte";

  const SYNC_COMMAND = "/trigger phase.sync";

  const status = $derived(look.status);
  const settings = $derived(look.settings);
  const online = $derived(status ? status.players.filter((p) => p.host || p.game_name).length : 1);

  /** Players: in an open room — from the core; without a room — only the host. */
  const players = $derived.by((): LookPlayer[] => {
    if (status) return status.players;
    const id = profiles.current?.device_id;
    if (!id) return [];
    const own = look.styleOf(id);
    return [
      {
        device_id: id,
        host: true,
        nickname: profiles.nickname,
        game_name: profiles.nickname,
        styled: true,
        label: own.prefix ?? "HOST",
        color: own.color,
        glow: own.glow,
      },
    ];
  });

  const MODE: Record<LookMode, { dot: string; text: (world: string) => string }> = {
    active: { dot: "dot-live", text: (w) => t("look.modeActive", { w }) },
    companion: { dot: "dot-live", text: (w) => t("look.modeCompanion", { w }) },
    waiting_reload: {
      dot: "dot-warn",
      text: (w) => t("look.modeWaiting", { w }),
    },
    no_world: { dot: "dot-busy", text: () => t("look.modeNoWorld") },
    unsupported: { dot: "dot-warn", text: () => t("look.modeUnsupported") },
    off: { dot: "", text: () => t("look.modeOff") },
  };
  const mode = $derived.by(() => {
    if (!settings.enabled) return { dot: "", text: MODE.off.text("") };
    if (!status) return { dot: "", text: t("look.modeLater") };
    return { dot: MODE[status.mode].dot, text: MODE[status.mode].text(status.world ?? "") };
  });

  // ---------------------------------------------------------------- header title
  let title = $state("");
  let titleFocused = $state(false);
  $effect(() => {
    const saved = settings.title ?? "";
    if (!titleFocused) title = saved;
  });
  const titleError = $derived.by(() => {
    const bad = [...title].find((ch) => /["\\§\p{Cc}]/u.test(ch));
    return bad === undefined ? null : t("look.badChar", { ch: bad });
  });
  const shownTitle = $derived((!titleError && title.trim()) || "PHASE");
  /** The Tab footer is shown until the host turns it off. */
  const footer = $derived(!settings.hide_footer);

  function saveTitle() {
    if (titleError) return;
    const next = title.trim().replace(/\s+/g, " ") || null;
    if (next !== (settings.title ?? null)) void look.update({ title: next });
  }

  // ---------------------------------------------------------------- players
  function sub(p: LookPlayer): string {
    if (!p.game_name) return t("look.subNotJoined");
    if (!p.styled) return t("look.subBadName");
    if (p.host) return t("look.subHost");
    if (p.nickname && p.nickname !== p.game_name) return t("look.subInPhase", { nickname: p.nickname });
    return shortId(p.device_id);
  }

  /** What will appear in the game without a custom prefix: for the host — the HOST label. */
  function fallback(p: LookPlayer): string | null {
    return p.host ? "HOST" : null;
  }

  // ---------------------------------------------------------------- slots
  /** Phase Companion is running in the open room's world: the game limit is lifted. */
  const companion = $derived(status?.mode === "companion");
  let slots = $state<number>(SLOTS.default);
  let slotsFocused = $state(false);
  let slotsTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const saved = settings.slots;
    if (!slotsFocused && slotsTimer === undefined) slots = saved;
  });
  const slotsValid = $derived(Number.isInteger(slots) && slots >= SLOTS.min && slots <= SLOTS.max);
  /** How many will fit now (or in a future room without the mod). */
  const capacity = $derived(roomCapacity(slotsValid ? slots : settings.slots, companion));
  const load = $derived<SlotsLoad>(slotsLoad(slotsValid ? slots : settings.slots));
  const LOAD: Record<SlotsLoad, { dot: string; key: Key }> = {
    light: { dot: "dot-live", key: "look.loadLight" },
    normal: { dot: "dot-live", key: "look.loadNormal" },
    heavy: { dot: "dot-warn", key: "look.loadHeavy" },
  };

  function saveSlots() {
    clearTimeout(slotsTimer);
    slotsTimer = undefined;
    if (!slotsValid || slots === settings.slots) return;
    void look.update({ slots });
  }

  /** The − and + buttons: change at once, save after a short pause (several presses — one write). */
  function stepSlots(delta: number) {
    const base = slotsValid ? slots : settings.slots;
    slots = Math.min(SLOTS.max, Math.max(SLOTS.min, base + delta));
    clearTimeout(slotsTimer);
    slotsTimer = setTimeout(saveSlots, 400);
  }

  // Left the tab — the unsaved title and slots are saved.
  onDestroy(() => {
    saveTitle();
    saveSlots();
  });
</script>

<section class="page flex flex-col gap-6" aria-labelledby="look-page-title">
  <header class="flex items-end justify-between gap-6">
    <div>
      <h1 id="look-page-title" class="page-title">{t("look.title")}</h1>
      <p class="page-lead">{t("look.modalLabel")}.</p>
    </div>
    <button
      type="button"
      role="switch"
      class="toggle-chip shrink-0"
      aria-checked={settings.enabled}
      aria-label={t("look.enabledLabel")}
      onclick={() => look.update({ enabled: !settings.enabled })}
      data-look-enabled
    >
      <span class="mini-switch" aria-hidden="true"></span>
      <span class="w-8 text-left" aria-hidden="true">{settings.enabled ? t("common.on") : t("common.off")}</span>
    </button>
  </header>

  <div class="card p-6">
  <p class="flex items-start gap-2 text-[13px]" aria-live="polite" data-look-mode>
    {#if mode.dot}<span class="dot {mode.dot} mt-[7px] shrink-0" aria-hidden="true"></span>{/if}
    <span>{mode.text}.</span>
  </p>
  {#if status?.error}
    <p class="note note-danger mt-3" role="alert"><span>{status.error}</span></p>
  {/if}
  {#if look.error}
    <p class="note note-danger mt-3" role="alert"><span>{look.error}</span></p>
  {/if}

  <fieldset class="contents" disabled={!settings.enabled}>
    <!-- Join header. -->
    <section class="look-block" aria-labelledby="look-bar">
      <h3 id="look-bar" class="look-title">{t("look.barTitle")}</h3>
      <div class="bossbar" role="img" aria-label={t("look.barLabel", { title: shownTitle })}>
        <p class="bossbar-text">
          <span class="font-bold" style:color={DEFAULT_ACCENT}>{shownTitle}</span>
          • {status?.code ?? t("look.roomCode")} • {t("look.online", { n: online })}
        </p>
        <span class="bossbar-line" aria-hidden="true"></span>
      </div>
      <div class="relative mt-3">
        <label class="sr-only" for="look-title-field">{t("look.titleLabel")}</label>
        <input
          id="look-title-field"
          class="field h-10 pr-14 text-[13px]"
          placeholder={t("look.titlePlaceholder")}
          maxlength={TITLE_MAX}
          spellcheck="false"
          autocomplete="off"
          bind:value={title}
          aria-invalid={titleError !== null}
          onfocus={() => (titleFocused = true)}
          onblur={() => {
            titleFocused = false;
            saveTitle();
          }}
          onkeydown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              saveTitle();
            }
          }}
          data-look-title
        />
        <span
          class="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 font-mono text-[11px] text-fg-3 tabular-nums"
          aria-hidden="true"
        >
          {[...title.trim()].length}/{TITLE_MAX}
        </span>
      </div>
      {#if titleError}<p class="mt-1.5 text-[12px] text-danger-text" role="alert">{titleError}</p>{/if}
      <!-- Tab list footer: written by Phase Companion. The host can remove it. -->
      <button
        type="button"
        role="switch"
        class="toggle-chip mt-2 -mx-2.5 w-[calc(100%+20px)] justify-between"
        aria-checked={footer}
        aria-describedby="look-footer-hint"
        onclick={() => look.update({ hide_footer: footer })}
        data-look-footer
      >
        {t("look.footerToggle")}
        <span class="mini-switch" aria-hidden="true"></span>
      </button>
      <p id="look-footer-hint" class="mt-1 text-[12px] text-fg-3">{t("look.footerHint")}</p>
    </section>

    <!-- Players: a row as in the game, color and prefix, highlight. -->
    <section class="look-block" aria-labelledby="look-players">
      <div class="flex items-center justify-between gap-3">
        <h3 id="look-players" class="look-title">{t("look.players")}</h3>
      </div>
      <ul class="mt-1" data-look-players>
        {#each players as p, i (p.device_id)}
          {@const name = p.game_name ?? p.nickname ?? t("look.player")}
          {@const tone = findColor(p.color)}
          <li class="flex flex-wrap items-center gap-x-3 gap-y-2 py-2.5">
            <Avatar name={p.nickname ?? p.game_name} seed={p.device_id} size="sm" />
            <div class="min-w-0 grow basis-[150px]">
              <p class="flex items-center gap-2 {p.styled ? '' : 'opacity-45'}">
                <GamePreview {name} label={p.styled ? p.label : null} color={p.styled ? p.color : null} glow={p.glow} size="sm" />
              </p>
              <p class="mt-1 truncate text-[12px] text-fg-3">{sub(p)}</p>
            </div>
            <div class="ml-auto flex shrink-0 items-center gap-2">
              <button
                type="button"
                class="look-btn"
                popovertarget="look-style-{i}"
                aria-label={t("look.colorTagLabel", { name, color: tone?.title ?? t("swatch.none") })}
                data-tone-btn
              >
                <span class="tone-dot" style:background={tone?.hex ?? "var(--text-3)"}></span>
                {t("look.colorTag")}
              </button>
              <button
                type="button"
                role="switch"
                class="look-btn"
                aria-checked={p.glow}
                aria-label={t("look.glowLabel", { name })}
                onclick={() => look.setStyle(p.device_id, { glow: !p.glow })}
                data-row-glow
              >
                {t("look.glowThroughWalls")}
                <span class="mini-switch" aria-hidden="true"></span>
              </button>
            </div>
          </li>
        {/each}
      </ul>
      {#if status && status.players.length === 1}
        <p class="mt-1 text-[12px] text-fg-3">{t("look.guestsHere")}</p>
      {/if}

      <!-- Style popovers: one per player. -->
      {#each players as p, i (p.device_id)}
        {@const name = p.game_name ?? p.nickname ?? t("look.player")}
        <Popover id="look-style-{i}" label={t("look.styleOf", { name })} width={300} height={300}>
          <div class="px-4 pt-4 pb-3.5">
            <StyleEditor
              {name}
              style={look.styleOf(p.device_id)}
              fallbackLabel={fallback(p)}
              onstyle={(patch) => look.setStyle(p.device_id, patch)}
            />
          </div>
        </Popover>
      {/each}
    </section>

    <!-- What guests may do. -->
    <section class="look-block">
      <button
        type="button"
        role="switch"
        class="toggle-chip -mx-2.5 w-[calc(100%+20px)] justify-between"
        aria-checked={settings.guest_glow}
        onclick={() => look.update({ guest_glow: !settings.guest_glow })}
        data-guest-glow
      >
        {t("look.guestGlow")}
        <span class="mini-switch" aria-hidden="true"></span>
      </button>
    </section>
  </fieldset>

  <!-- Room slots: outside styling — works even when styling is off. Free. -->
  <section class="look-block" aria-labelledby="look-slots" data-look-slots>
    <div class="flex items-center justify-between gap-3">
      <div class="min-w-0">
        <h3 id="look-slots" class="look-title">{t("look.slots")}</h3>
        <p class="mt-0.5 text-[12px] text-fg-3">{t("look.slotsSub", { min: SLOTS.min, max: SLOTS.max })}</p>
      </div>
      <div class="slots-stepper" role="group" aria-label={t("look.slots")}>
        <button
          type="button"
          class="slots-step"
          aria-label={t("look.fewer")}
          disabled={slotsValid && slots <= SLOTS.min}
          onclick={() => stepSlots(-1)}
        >
          <Minus size={14} strokeWidth={2.2} aria-hidden="true" />
        </button>
        <label class="sr-only" for="look-slots-field">{t("look.slotsFieldLabel", { min: SLOTS.min, max: SLOTS.max })}</label>
        <input
          id="look-slots-field"
          class="slots-field tabular-nums"
          type="number"
          inputmode="numeric"
          min={SLOTS.min}
          max={SLOTS.max}
          step="1"
          bind:value={slots}
          aria-invalid={!slotsValid}
          aria-describedby="look-slots-hint"
          onfocus={() => (slotsFocused = true)}
          onblur={() => {
            slotsFocused = false;
            saveSlots();
            if (!slotsValid) slots = settings.slots;
          }}
          onkeydown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              saveSlots();
            }
          }}
          data-slots-field
        />
        <button
          type="button"
          class="slots-step"
          aria-label={t("look.more")}
          disabled={slotsValid && slots >= SLOTS.max}
          onclick={() => stepSlots(1)}
        >
          <Plus size={14} strokeWidth={2.2} aria-hidden="true" />
        </button>
      </div>
    </div>
    <div id="look-slots-hint" class="mt-2.5 flex flex-col gap-1.5 text-[12px] leading-snug" aria-live="polite">
      {#if !slotsValid}
        <p class="text-danger-text" role="alert">{t("look.slotsRange", { min: SLOTS.min, max: SLOTS.max })}</p>
      {/if}
      <p class="flex items-start gap-2 text-fg-2">
        <span class="dot {LOAD[load].dot} mt-[5px] shrink-0" aria-hidden="true"></span>
        <span>{t(LOAD[load].key)}</span>
      </p>
      <p class="text-fg-3" data-slots-effective>
        {#if companion}
          {t("look.capCompanion", { n: capacity })}
        {:else if status}
          {t("look.capNoCompanion", { vanilla: SLOTS.vanilla, n: capacity })}
        {:else}
          {t("look.capFuture", { vanilla: SLOTS.vanilla })}
        {/if}
      </p>
    </div>
  </section>

  <section class="look-block">
    <CompanionCard />
  </section>

  <!-- Joining the world: outside styling — works even when styling is off. Free. -->
  <section class="look-block">
    <AccountCheckToggle />
  </section>

  <div class="look-block flex flex-wrap items-center justify-between gap-3">
    <p class="min-w-0 basis-[320px] grow text-[12px] text-fg-3">{t("look.applyNote")}</p>
    <CopyButton text={SYNC_COMMAND} label={t("look.copyCommand", { command: SYNC_COMMAND })} class="btn" />
  </div>
  </div>
</section>

<style>
  .look-block {
    margin-top: 16px;
    border-top: 1px solid var(--border-subtle);
    padding-top: 14px;
  }

  .look-title {
    color: var(--text-1);
    font-size: 13px;
    font-weight: 600;
  }

  /* The header, as in the game: text centered and a blue bar under it. The plate is always dark —
     it is a game preview. */
  .bossbar {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 7px;
    margin-top: 10px;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 12px;
    background: #0b0e17;
    padding: 11px 16px 12px;
  }

  .bossbar-text {
    max-width: 100%;
    overflow: hidden;
    color: #ffffff;
    font-family: var(--font-mono);
    font-size: 13px;
    line-height: 1.2;
    text-overflow: ellipsis;
    text-shadow: 1px 1px 0 rgba(0, 0, 0, 0.75);
    white-space: nowrap;
  }

  .bossbar-line {
    width: min(100%, 260px);
    height: 5px;
    border-radius: 2px;
    background: linear-gradient(90deg, #1d4ed8, #3b82f6);
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.6);
  }

  /* "Room slots": a 10px field with − and + buttons of 8px inside, the number centered. */
  .slots-stepper {
    display: flex;
    flex-shrink: 0;
    align-items: center;
    gap: 2px;
    border: 1px solid var(--border-input);
    border-radius: 10px;
    background: var(--bg-input);
    padding: 3px;
  }

  .slots-step {
    display: grid;
    width: 30px;
    height: 30px;
    place-items: center;
    border-radius: 8px;
    color: var(--text-2);
    transition:
      background-color 200ms var(--ease-ui),
      color 200ms var(--ease-ui);
  }

  .slots-step:hover:not(:disabled) {
    background: var(--bg-card-hover);
    color: var(--text-1);
  }

  .slots-step:disabled {
    opacity: 0.4;
  }

  .slots-field {
    width: 44px;
    height: 30px;
    border-radius: 8px;
    background: transparent;
    color: var(--text-1);
    font-size: 15px;
    font-weight: 600;
    text-align: center;
    appearance: textfield;
    -moz-appearance: textfield;
  }

  .slots-field::-webkit-inner-spin-button,
  .slots-field::-webkit-outer-spin-button {
    margin: 0;
    appearance: none;
  }

  .slots-field[aria-invalid="true"] {
    color: var(--danger-text);
  }

  /* A colored dot on the "Color and tag" button: always filled (without a color — gray). */
  .tone-dot {
    width: 16px;
    height: 16px;
    flex-shrink: 0;
    border-radius: 50%;
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.28);
  }
</style>
