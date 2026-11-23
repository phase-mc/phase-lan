<script lang="ts">
  // Device ID in groups of 8 characters in rounded tiles: easier to compare by ear
  // or by eye ("third group: …"). A 4 × 2 grid.
  import { t } from "../lib/i18n/index.svelte";

  let { id, compact = false }: { id: string; compact?: boolean } = $props();

  const groups = $derived(id.match(/.{1,8}/g) ?? []);
</script>

<p class="grid grid-cols-4 {compact ? 'gap-1.5' : 'gap-2'}" aria-label={t("device.idLabel", { id })}>
  {#each groups as group, i (i)}
    <span
      class="rounded-[8px] border border-line bg-input text-center font-mono font-medium text-fg {compact
        ? 'px-1.5 py-1 text-[12px]'
        : 'px-2 py-1.5 text-[13px]'}"
      aria-hidden="true">{group}</span
    >
  {/each}
</p>
