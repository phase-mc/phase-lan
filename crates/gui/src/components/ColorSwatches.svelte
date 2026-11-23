<script lang="ts">
  // Phase palette: a strict choice from twelve brand colors — no RGB wheel or HEX
  // field. The first swatch is "no color". A radio group: arrows move across swatches,
  // the selected one is outlined with an icy ring with a gap.
  import X from "@lucide/svelte/icons/x";
  import { PALETTE } from "../lib/core";
  import { t } from "../lib/i18n/index.svelte";

  interface Props {
    /** Selected color (`null` — no color). */
    value: string | null;
    onchange: (color: string | null) => void;
    disabled?: boolean;
    /** Group label for screen readers. */
    label?: string;
  }

  let { value, onchange, disabled = false, label: given }: Props = $props();
  const label = $derived(given ?? t("swatch.group"));

  const uid = $props.id();
</script>

<div class="swatches" role="radiogroup" aria-label={label} data-swatches>
  <label class="swatch swatch-none">
    <input
      type="radio"
      class="sr-only"
      name="{uid}-color"
      checked={value === null}
      {disabled}
      onchange={() => onchange(null)}
    />
    <X size={12} strokeWidth={2.4} aria-hidden="true" />
    <span class="sr-only">{t("swatch.none")}</span>
  </label>
  {#each PALETTE as color (color.id)}
    <label class="swatch" style:--tone={color.hex} data-color={color.id}>
      <input
        type="radio"
        class="sr-only"
        name="{uid}-color"
        checked={value === color.id}
        {disabled}
        onchange={() => onchange(color.id)}
      />
      <span class="sr-only">{color.title}</span>
    </label>
  {/each}
</div>

<style>
  .swatches {
    display: grid;
    grid-template-columns: repeat(7, 26px);
    gap: 9px;
  }

  .swatch {
    position: relative;
    display: grid;
    width: 26px;
    height: 26px;
    cursor: pointer;
    place-items: center;
    border-radius: 50%;
    background: var(--tone);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.35),
      inset 0 0 0 1px rgba(0, 0, 0, 0.18);
    transition: box-shadow 200ms var(--ease-ui);
  }

  .swatch-none {
    border: 1px dashed var(--border-strong);
    background: transparent;
    box-shadow: none;
    color: var(--text-3);
  }

  .swatch:hover {
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.35),
      inset 0 0 0 1px rgba(0, 0, 0, 0.18),
      0 0 0 2px var(--bg-panel),
      0 0 0 3px var(--border-strong);
  }

  .swatch-none:hover {
    box-shadow:
      0 0 0 2px var(--bg-panel),
      0 0 0 3px var(--border-strong);
    color: var(--text-1);
  }

  /* Selected: an icy ring with a 2px gap. */
  .swatch:has(:checked),
  .swatch:has(:checked):hover {
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.35),
      inset 0 0 0 1px rgba(0, 0, 0, 0.18),
      0 0 0 2px var(--bg-panel),
      0 0 0 4px var(--sky);
  }

  .swatch-none:has(:checked),
  .swatch-none:has(:checked):hover {
    box-shadow:
      0 0 0 2px var(--bg-panel),
      0 0 0 4px var(--sky);
    color: var(--text-1);
  }

  .swatch:has(:focus-visible) {
    outline: 2px solid var(--focus-border);
    outline-offset: 4px;
  }

  .swatch:has(:disabled) {
    cursor: not-allowed;
    opacity: 0.45;
  }
</style>
