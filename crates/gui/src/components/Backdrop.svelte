<script lang="ts">
  // The window canvas — the chosen background across the whole screen ("Settings" tab). The Phase LAN window is
  // a control panel: text and cards use theme tokens, so the picture lies under a veil in the theme
  // color. On "Host" and "Join" the picture is more visible, on utility tabs the veil is
  // denser — lists and small text stay readable (contrast ≥ 4.5:1 on any picture).
  // The layer is static: rasterized once, no frames at rest. A background change is a single fade.
  import { onMount } from "svelte";
  import { background } from "../lib/background.svelte";
  import { nav } from "../lib/nav.svelte";

  const scene = $derived(nav.tab === "host" || nav.tab === "join");
  const src = $derived(background.src);

  onMount(() => void background.init());
</script>

<div class="backdrop" data-scene={scene ? "" : undefined} aria-hidden="true" data-backdrop>
  {#key src}
    {#if src}<img class="art" {src} alt="" decoding="async" />{/if}
  {/key}
  {#if src}<div class="veil"></div>{/if}
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: -1;
    overflow: hidden;
    pointer-events: none;
    background: var(--bg-app);
  }

  .art,
  .veil {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }

  .art {
    object-fit: cover;
    object-position: 62% 50%;
    animation: art-in 260ms var(--ease-ui) backwards;
  }

  @keyframes art-in {
    from {
      opacity: 0;
    }
  }

  .veil {
    transition: background 260ms var(--ease-ui);
  }

  /* "Host" and "Join": the picture is visible, but the theme color stays under the text. */
  .backdrop[data-scene] .veil {
    background:
      linear-gradient(90deg, color-mix(in srgb, var(--bg-app) 20%, transparent) 0%, transparent 60%),
      color-mix(in srgb, var(--bg-app) 74%, transparent);
  }

  /* Utility tabs: the same picture under a dense veil in the theme color. */
  .backdrop:not([data-scene]) .veil {
    background: color-mix(in srgb, var(--bg-app) 88%, transparent);
  }

  @media (prefers-reduced-motion: reduce) {
    .art {
      animation: none;
    }
  }
</style>
