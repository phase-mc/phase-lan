<script lang="ts">
  // Player style editor: a live preview "[PREFIX] Nickname" as in Minecraft, a palette of
  // twelve Phase colors, a prefix field with a counter and (if allowed) a silhouette
  // highlight toggle. Color and highlight are sent at once, the prefix — when the user
  // has finished typing (a pause, Enter or leaving the field); an invalid prefix isn't sent.
  import { untrack } from "svelte";
  import { PREFIX_MAX, prefixError, type PlayerStyle } from "../lib/core";
  import { t } from "../lib/i18n/index.svelte";
  import ColorSwatches from "./ColorSwatches.svelte";
  import GamePreview from "./GamePreview.svelte";

  interface Props {
    /** In-game nickname for the preview. */
    name: string;
    /** The given style. */
    style: PlayerStyle;
    /** The label that will appear in the game without a custom prefix (HOST). */
    fallbackLabel?: string | null;
    /** The color that will appear in the game without a custom color. */
    fallbackColor?: string | null;
    /** Show the highlight toggle. */
    showGlow?: boolean;
    disabled?: boolean;
    onstyle: (patch: Partial<PlayerStyle>) => void;
  }

  let {
    name,
    style,
    fallbackLabel = null,
    fallbackColor = null,
    showGlow = true,
    disabled = false,
    onstyle,
  }: Props = $props();

  const uid = $props.id();
  /** How long to wait after the last letter before sending the prefix. */
  const TYPING_PAUSE_MS = 450;

  let draft = $state(untrack(() => style.prefix ?? ""));
  let focused = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;

  // The prefix was changed from outside (another window, a core reply) — pick it up if we're not typing.
  $effect(() => {
    const saved = style.prefix ?? "";
    if (!focused) draft = saved;
  });

  const error = $derived(prefixError(draft));
  const length = $derived([...draft.trim()].length);
  const shownLabel = $derived(error ? fallbackLabel : draft.trim() || fallbackLabel);
  const shownColor = $derived(style.color ?? fallbackColor);

  function commit() {
    clearTimeout(timer);
    if (error) return;
    const prefix = draft.trim() || null;
    if (prefix !== (style.prefix ?? null)) onstyle({ prefix });
  }

  function oninput() {
    clearTimeout(timer);
    timer = setTimeout(commit, TYPING_PAUSE_MS);
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key !== "Enter") return;
    event.preventDefault();
    event.stopPropagation();
    commit();
  }
</script>

<div class="flex flex-col gap-3.5" data-style-editor>
  <div class="flex items-center justify-between gap-3">
    <GamePreview {name} label={shownLabel} color={shownColor} glow={showGlow && style.glow} />
    <span class="shrink-0 text-[11px] text-fg-3">{t("nick.likeMinecraft")}</span>
  </div>

  <ColorSwatches value={style.color} {disabled} onchange={(color) => onstyle({ color })} />

  <div>
    <label class="sr-only" for="{uid}-prefix">{t("style.prefixLabel")}</label>
    <div class="relative">
      <input
        id="{uid}-prefix"
        class="field h-10 pr-12 text-[13px]"
        placeholder={fallbackLabel ? t("style.prefixNow", { label: fallbackLabel }) : t("style.prefixExample")}
        maxlength={PREFIX_MAX}
        spellcheck="false"
        autocomplete="off"
        bind:value={draft}
        {disabled}
        aria-invalid={error !== null}
        aria-describedby={error ? `${uid}-error` : undefined}
        {oninput}
        {onkeydown}
        onfocus={() => (focused = true)}
        onblur={() => {
          focused = false;
          commit();
        }}
      />
      <span
        class="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 font-mono text-[11px] text-fg-3 tabular-nums"
        aria-hidden="true"
      >
        {length}/{PREFIX_MAX}
      </span>
    </div>
    {#if error}
      <p id="{uid}-error" class="mt-1.5 text-[12px] text-danger-text" role="alert">{error}</p>
    {/if}
  </div>

  {#if showGlow}
    <button
      type="button"
      role="switch"
      class="toggle-chip -mx-2.5 w-[calc(100%+20px)] justify-between"
      aria-checked={style.glow}
      {disabled}
      onclick={() => onstyle({ glow: !style.glow })}
      data-glow-toggle
    >
      {t("look.glowThroughWalls")}
      <span class="mini-switch" aria-hidden="true"></span>
    </button>
  {/if}
</div>
