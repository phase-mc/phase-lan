<script lang="ts">
  // The icon on the right of the main button (.btn-pill): "copy", "download". Without an icon —
  // nothing: the main button is just a solid rectangle with text.
  import type { Snippet } from "svelte";

  let { children }: { children?: Snippet } = $props();
</script>

{#if children}
  <span class="orb" aria-hidden="true">{@render children()}</span>
{/if}
