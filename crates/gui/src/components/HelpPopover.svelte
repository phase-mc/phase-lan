<script lang="ts">
  // A secondary capsule next to the main button ("How do I connect the world?") and its glass
  // popover: three steps and voice chat. The steps and voice text come from the tab — host and
  // guest have their own. compact — a round "?" button of the same height (when the row is tight).
  import CircleHelp from "@lucide/svelte/icons/circle-help";
  import Mic from "@lucide/svelte/icons/mic";
  import { t } from "../lib/i18n/index.svelte";
  import Popover from "./Popover.svelte";

  interface Props {
    id: string;
    /** Label on the button and the popover title. */
    label?: string;
    steps: string[];
    voice: { title: string; text: string };
    /** A small-print line under the steps (for example, the host's game version). */
    note?: string | null;
    compact?: boolean;
  }

  let { id, label: given, steps, voice, note = null, compact = false }: Props = $props();
  const label = $derived(given ?? t("help.howToJoin"));
  let open = $state(false);
</script>

{#if compact}
  <button
    type="button"
    class="btn-ghost-pill w-[50px] px-0"
    popovertarget={id}
    aria-expanded={open}
    aria-label={label}
  >
    <CircleHelp size={19} strokeWidth={2} aria-hidden="true" />
  </button>
{:else}
  <button type="button" class="btn-ghost-pill" popovertarget={id} aria-expanded={open}>{label}</button>
{/if}

<Popover {id} label={label.replace(/\?$/, "")} align="start" width={388} height={360} bind:open>
  <div class="pop-head">
    <p class="text-[16px] leading-tight font-semibold text-fg">{label.replace(/\?$/, "")}</p>
    <span class="badge">{t("count.steps", { n: steps.length })}</span>
  </div>
  <ol class="space-y-3.5 px-[18px] pb-4">
    {#each steps as text, i (i)}
      <li class="flex items-start gap-3">
        <span
          class="grid size-6 shrink-0 place-items-center rounded-full bg-accent-soft text-[12px] font-semibold text-accent-text tabular-nums"
          aria-hidden="true">{i + 1}</span
        >
        <span class="pt-0.5 text-[13px] leading-relaxed text-fg">{text}</span>
      </li>
    {/each}
  </ol>
  {#if note}
    <p class="-mt-1 px-[18px] pb-4 text-[12px] text-fg-2">{note}</p>
  {/if}
  <div class="pop-section flex items-start gap-3">
    <span class="icon-tile size-9 rounded-full"><Mic size={16} strokeWidth={2} aria-hidden="true" /></span>
    <div class="min-w-0">
      <p class="text-[13px] font-semibold text-fg">
        {t("help.voiceChat")} <span class="text-fg-3" aria-hidden="true">·</span>
        <span class="font-mono font-medium text-accent-text">{voice.title}</span>
      </p>
      <p class="mt-1 text-[12px] leading-relaxed text-fg-2">{voice.text}</p>
    </div>
  </div>
</Popover>
