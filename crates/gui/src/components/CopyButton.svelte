<script lang="ts">
  import Check from "@lucide/svelte/icons/check";
  import Copy from "@lucide/svelte/icons/copy";
  import { copyText } from "../lib/clipboard";
  import { t } from "../lib/i18n/index.svelte";
  import Orb from "./Orb.svelte";

  interface Props {
    /** What to copy. */
    text: string;
    label?: string;
    class?: string;
    /** Icon size, px. */
    icon?: number;
    /** White capsule (.btn-pill): the icon lives in a round orb slot on the right. */
    orb?: boolean;
    disabled?: boolean;
  }

  let {
    text,
    label,
    class: klass = "btn",
    icon = 16,
    orb = false,
    disabled = false,
  }: Props = $props();

  let state = $state<"idle" | "done" | "failed">("idle");
  let timer: ReturnType<typeof setTimeout> | undefined;

  async function copy() {
    state = (await copyText(text)) ? "done" : "failed";
    clearTimeout(timer);
    timer = setTimeout(() => (state = "idle"), 1800);
  }
</script>

{#snippet glyph()}
  {#if state === "done"}
    <Check size={icon} strokeWidth={2.4} aria-hidden="true" />
  {:else}
    <Copy size={icon} strokeWidth={2.2} aria-hidden="true" />
  {/if}
{/snippet}

<button type="button" class={klass} onclick={copy} {disabled}>
  {#if !orb}{@render glyph()}{/if}
  <span aria-live="polite">
    {state === "done" ? t("copy.done") : state === "failed" ? t("copy.failed") : (label ?? t("copy.label"))}
  </span>
  {#if orb}<Orb>{@render glyph()}</Orb>{/if}
</button>
