<script lang="ts">
  // Your own nickname — the first segment of the profile capsule in the top row: an avatar with the device
  // state dot, the nickname (preceded by your prefix, if any), a pencil. A click opens editing
  // right in the capsule and the panel below it:
  //   nickname — Enter saves, Esc cancels, a click outside saves a changed nickname (an empty one —
  //   cancel); during a session the nickname doesn't change: the other side already got the old one;
  //   "In-game style" — a live preview "[PREFIX] Nickname" as in Minecraft and what can be
  //   changed now: the host (in a room or without one) — their own color, prefix and highlight in their
  //   room; a guest — highlight on themselves, if the host allowed it. All free.
  import Check from "@lucide/svelte/icons/check";
  import Pencil from "@lucide/svelte/icons/pencil";
  import { tick } from "svelte";
  import { app } from "../lib/app.svelte";
  import { DEFAULT_ACCENT, findColor, NICKNAME_MAX } from "../lib/core";
  import { t } from "../lib/i18n/index.svelte";
  import { look } from "../lib/look.svelte";
  import { cleanNickname, nicknameError, profiles } from "../lib/profiles.svelte";
  import Avatar from "./Avatar.svelte";
  import GamePreview from "./GamePreview.svelte";
  import StyleEditor from "./StyleEditor.svelte";

  interface Props {
    /** A session is running — the nickname can't be changed. */
    locked: boolean;
    /** Class of the device state dot and its label. */
    dot: string;
    dotLabel: string;
  }

  let { locked, dot, dotLabel }: Props = $props();

  const uid = $props.id();
  const nickname = $derived(profiles.nickname);
  const deviceId = $derived(profiles.current?.device_id ?? null);

  let editing = $state(false);
  let draft = $state("");
  let error = $state<string | null>(null);
  let saving = $state(false);
  /** A short confirmation after saving: a check mark instead of the pencil. */
  let saved = $state(false);
  let savedTimer: ReturnType<typeof setTimeout> | undefined;
  let root = $state<HTMLDivElement>();
  let input = $state<HTMLInputElement>();
  let button = $state<HTMLButtonElement>();

  const length = $derived([...cleanNickname(draft)].length);

  // ---------------------------------------------------------------- in-game style
  /** Which role we are in now: it determines what can be changed. */
  const role = $derived(app.active ? app.role : null);
  /** The host's row in the open room. */
  const hostRow = $derived(role === "host" ? (look.players.find((p) => p.host) ?? null) : null);
  /** Own style in own room (stored on the host by device ID). */
  const own = $derived(deviceId ? look.styleOf(deviceId) : null);

  /** How we look in the game right now (or will look in our own room). */
  const shown = $derived.by((): { name: string; label: string | null; color: string | null; glow: boolean } => {
    const fallbackName = editing && !locked ? cleanNickname(draft) || nickname : nickname;
    if (role === "guest") {
      const mine = look.mine;
      return {
        name: mine?.game_name ?? nickname,
        label: mine?.prefix ?? null,
        color: mine?.color ?? null,
        glow: mine?.glow ?? false,
      };
    }
    if (hostRow) {
      return {
        name: hostRow.game_name ?? nickname,
        label: hostRow.label,
        color: hostRow.color,
        glow: hostRow.glow,
      };
    }
    return {
      name: fallbackName,
      label: own?.prefix ?? "HOST",
      color: own?.color ?? null,
      glow: own?.glow ?? false,
    };
  });

  /** Own prefix at rest — before the nickname in the capsule (the HOST label isn't shown). */
  const chipPrefix = $derived(shown.label && shown.label !== "HOST" ? shown.label : null);

  // ---------------------------------------------------------------- nickname editing
  async function open() {
    if (!nickname) return;
    draft = nickname;
    error = null;
    look.error = null;
    editing = true;
    await tick();
    if (locked) return root?.querySelector<HTMLElement>("[data-nick-panel]")?.focus();
    input?.focus();
    input?.select();
  }

  async function close(refocus: boolean) {
    editing = false;
    error = null;
    if (!refocus) return;
    await tick();
    button?.focus();
  }

  async function save(refocus: boolean) {
    if (saving) return;
    if (locked || cleanNickname(draft) === nickname) return close(refocus);
    saving = true;
    error = await profiles.setNickname(draft);
    saving = false;
    if (error) {
      // The field becomes available again only after a re-render.
      await tick();
      input?.focus();
      return;
    }
    saved = true;
    clearTimeout(savedTimer);
    savedTimer = setTimeout(() => (saved = false), 1800);
    await close(refocus);
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      void close(true);
    } else if (event.key === "Enter" && event.target === input) {
      event.preventDefault();
      void save(true);
    }
  }

  /** Focus left the capsule and the panel — this is a "click outside": a changed valid nickname
   *  is saved, anything else is a cancel. Moving between the field and the style panel is not leaving. */
  function onfocusout(event: FocusEvent) {
    if (!editing || saving) return;
    // The pressed button (the pencil) vanished from the DOM along with focus — this is not a "click outside".
    if (event.target instanceof Node && !event.target.isConnected) return;
    const next = event.relatedTarget;
    if (next instanceof Node && root?.contains(next)) return;
    if (locked || cleanNickname(draft) === nickname || nicknameError(draft)) void close(false);
    else void save(false);
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="relative h-full" bind:this={root} {onfocusout} {onkeydown}>
  {#if editing}
    <div class="nick-edit flex h-full w-[236px] items-center gap-2 rounded-full pr-3 pl-[5px]" data-invalid={error !== null}>
      <Avatar name={draft} seed={deviceId} size="sm" />
      <label class="sr-only" for="{uid}-nick">{t("nick.yours")}</label>
      <input
        id="{uid}-nick"
        bind:this={input}
        bind:value={draft}
        class="min-w-0 grow bg-transparent text-[13px] font-semibold text-fg outline-none disabled:text-fg-2"
        maxlength={NICKNAME_MAX}
        spellcheck="false"
        autocomplete="off"
        disabled={saving || locked}
        aria-invalid={error !== null}
        aria-describedby="{uid}-hint"
      />
      {#if !locked}
        <span class="shrink-0 font-mono text-[11px] text-fg-3 tabular-nums" aria-hidden="true">
          {length}/{NICKNAME_MAX}
        </span>
      {/if}
    </div>
    <div
      class="nick-panel fade-in absolute top-[calc(100%+12px)] left-0 z-40 w-[304px] rounded-[18px] border border-line-strong bg-panel outline-none"
      style="box-shadow: var(--modal-shadow)"
      tabindex="-1"
      role="group"
      aria-label={t("nick.panel")}
      data-nick-panel
    >
      <div id="{uid}-hint" class="px-4 pt-3.5 pb-3">
        {#if error}
          <p class="text-[12px] text-danger-text" role="alert">{error}</p>
        {:else if locked}
          <p class="text-[12px] text-fg-2">{t("nick.locked")}</p>
        {:else}
          <p class="text-[12px] text-fg-2">{t("nick.hint")}</p>
          <p class="mt-2 flex items-center gap-3 text-[11px] text-fg-3">
            <span><span class="kbd">Enter</span> {t("common.save")}</span>
            <span><span class="kbd">Esc</span> {t("nick.keyCancel")}</span>
          </p>
        {/if}
      </div>

      <section class="border-t border-line px-4 pt-3 pb-4" aria-labelledby="{uid}-style" data-my-style>
        <p id="{uid}-style" class="mb-3 text-[13px] font-semibold text-fg">{t("nick.style")}</p>

        {#if role === "guest"}
          <!-- Guest: the host decides everything; only highlight can be toggled on yourself. -->
          {@const mine = look.mine}
          <div class="flex items-center justify-between gap-3">
            <GamePreview name={shown.name} label={shown.label} color={shown.color} glow={shown.glow} />
            <span class="shrink-0 text-[11px] text-fg-3">{t("nick.likeMinecraft")}</span>
          </div>
          {#if !mine}
            <p class="mt-3 text-[12px] text-fg-3">{t("nick.hostOld")}</p>
          {:else if !mine.enabled}
            <p class="mt-3 text-[12px] text-fg-3">{t("nick.hostOff")}</p>
          {:else}
            <button
              type="button"
              role="switch"
              class="toggle-chip -mx-2.5 mt-3 w-[calc(100%+20px)] justify-between"
              aria-checked={mine.glow}
              disabled={!mine.can_glow}
              onclick={() => look.setOwnGlow(!mine.glow)}
              data-own-glow
            >
              {t("look.glowThroughWalls")}
              <span class="mini-switch" aria-hidden="true"></span>
            </button>
            <p class="mt-2 text-[12px] text-fg-3">
              {mine.can_glow ? t("nick.hostSetsStyle") : t("nick.hostNoGlow")}
            </p>
          {/if}
        {:else if deviceId && own}
          <!-- Host (or without a session — for their future room): own color, prefix and
               highlight. In an open room they change on the fly. -->
          <StyleEditor
            name={shown.name}
            style={own}
            fallbackLabel="HOST"
            onstyle={(patch) => look.setStyle(deviceId, patch)}
          />
          <p class="mt-2.5 text-[12px] text-fg-3">
            {t("nick.ownRoomNote")}
          </p>
        {:else}
          <!-- The profile is still being read: preview only. -->
          <div class="flex items-center justify-between gap-3">
            <GamePreview name={shown.name} label={shown.label} color={shown.color} glow={shown.glow} />
            <span class="shrink-0 text-[11px] text-fg-3">{t("nick.likeMinecraft")}</span>
          </div>
        {/if}
        {#if look.error}
          <p class="mt-2 text-[12px] text-danger-text" role="alert">{look.error}</p>
        {/if}
      </section>
    </div>
  {:else}
    <!-- Nickname and pencil are two buttons: the pencil opens nickname editing right away. -->
    <div class="flex h-full items-center">
      <button
        bind:this={button}
        type="button"
        class="capsule-btn max-w-[210px] pr-2 pl-[5px]"
        onclick={open}
        disabled={!nickname}
        style="--avatar-ring: var(--bg-app)"
        aria-label={nickname ? t("nick.chipLabel", { nickname, state: dotLabel }) : t("nick.loading")}
        aria-expanded="false"
        data-nick
      >
        <Avatar name={nickname || null} seed={deviceId} size="sm" {dot} />
        {#if nickname}
          {#if chipPrefix}
            <span
              class="shrink-0 text-[11px] font-semibold"
              style:color={findColor(shown.color)?.hex ?? DEFAULT_ACCENT}
              data-nick-prefix
            >{chipPrefix}</span>
          {/if}
          <span class="min-w-0 truncate font-semibold">{nickname}</span>
        {:else}
          <span class="h-2.5 w-20 rounded-full bg-line-strong" aria-hidden="true"></span>
        {/if}
      </button>
      {#if nickname}
        <button
          type="button"
          class="capsule-btn w-8 justify-center px-0 text-fg-2 hover:text-fg"
          onclick={open}
          aria-label={saved ? t("nick.saved") : t("nick.edit")}
          aria-expanded="false"
          data-nick-pencil
        >
          {#if saved}
            <Check size={14} strokeWidth={2.2} class="text-live-text" aria-hidden="true" />
          {:else}
            <Pencil size={13} strokeWidth={2.2} aria-hidden="true" />
          {/if}
        </button>
      {/if}
    </div>
  {/if}
</div>

<style>
  .nick-edit {
    background: var(--bg-input);
    box-shadow:
      inset 0 0 0 1px var(--focus-border),
      var(--focus-ring);
    transition: box-shadow 200ms var(--ease-ui);
  }

  .nick-edit[data-invalid="true"] {
    box-shadow:
      inset 0 0 0 1px var(--danger),
      0 0 0 3px var(--danger-soft);
  }
</style>
