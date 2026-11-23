<script lang="ts">
  // A live preview of the player line "[PREFIX] Nickname" as Minecraft draws it in Tab and above the
  // head. This is a game preview, not interface colors: the plate is always dark, the nick is in
  // the real in-game team color, the prefix in the exact palette color, the letters have the in-game
  // shadow. Silhouette highlight is shown as an outline around the nick.
  import { DEFAULT_ACCENT, findColor } from "../lib/core";
  import { t } from "../lib/i18n/index.svelte";

  interface Props {
    /** In-game nickname. */
    name: string;
    /** Prefix without brackets (`null` — no prefix). */
    label?: string | null;
    /** Palette color id (`null` — a plain white nick). */
    color?: string | null;
    /** The silhouette is highlighted through walls. */
    glow?: boolean;
    size?: "sm" | "md";
  }

  let { name, label = null, color = null, glow = false, size = "md" }: Props = $props();

  const tone = $derived(findColor(color));
  const nameHex = $derived(tone?.mcHex ?? "#FFFFFF");
  const prefixHex = $derived(tone?.hex ?? DEFAULT_ACCENT);
  const spoken = $derived(
    t(glow ? "preview.spokenGlow" : "preview.spoken", { line: `${label ? `[${label}] ` : ""}${name}` }),
  );
</script>

<span class="game-line" data-size={size} data-glow={glow} role="img" aria-label={spoken} data-game-preview>
  {#if label}<span class="mc" style:color={prefixHex}>[{label}]</span>{/if}
  <span class="mc mc-name" style:color={nameHex} style:--outline={nameHex}>{name}</span>
</span>

<style>
  .game-line {
    display: inline-flex;
    max-width: 100%;
    min-width: 0;
    height: 30px;
    align-items: center;
    gap: 0.6ch;
    border: 1px solid rgba(255, 255, 255, 0.08);
    border-radius: 8px;
    background: #0b0e17;
    padding: 0 11px;
    font-family: var(--font-mono);
    font-size: 13px;
    font-weight: 500;
    line-height: 1;
    white-space: nowrap;
  }

  .game-line[data-size="sm"] {
    height: 24px;
    border-radius: 7px;
    padding: 0 8px;
    font-size: 12px;
  }

  /* Letter shadow, as in the game: the same color but at a quarter of the brightness, offset down-right. */
  .mc {
    flex-shrink: 0;
    text-shadow: 1px 1px 0 color-mix(in srgb, currentColor 25%, #000);
  }

  /* 2px of room on the right and bottom: the text shadow is offset by a pixel, and without
     it `overflow` would clip the last letter together with its shadow. */
  .mc-name {
    min-width: 0;
    flex-shrink: 1;
    margin: 0 -2px -2px 0;
    padding: 0 2px 2px 0;
    overflow: hidden;
    text-overflow: ellipsis;
    transition: text-shadow 200ms var(--ease-ui);
  }

  /* Highlight outline — in the team color, like the Glowing effect. */
  [data-glow="true"] .mc-name {
    text-shadow:
      0 0 1px var(--outline),
      0 0 7px var(--outline),
      0 0 14px color-mix(in srgb, var(--outline) 55%, transparent);
  }
</style>
