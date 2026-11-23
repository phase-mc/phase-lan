<script lang="ts">
  // Error of the last action or session — a red card above the tab content.
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import X from "@lucide/svelte/icons/x";
  import { app } from "../lib/app.svelte";
  import { t } from "../lib/i18n/index.svelte";
  import { profiles } from "../lib/profiles.svelte";

  const text = $derived(
    app.error ??
      (profiles.current?.error
        ? t("error.profile", { name: profiles.current.name, error: profiles.current.error })
        : null) ??
      profiles.error,
  );
</script>

{#if text}
  <div
    role="alert"
    class="fade-in mx-auto mt-2 flex w-[min(860px,calc(100%-2*var(--gutter)))] shrink-0 items-start gap-3 rounded-[14px] border px-4 py-3"
    style="border-color: color-mix(in srgb, var(--danger) 35%, transparent); background: var(--danger-soft);"
  >
    <TriangleAlert size={18} strokeWidth={2} class="mt-0.5 shrink-0 text-danger-text" aria-hidden="true" />
    <div class="min-w-0 grow">
      <p class="text-[13px] font-semibold text-danger-text">{t("error.title")}</p>
      <p class="mt-0.5 text-[13px] text-fg first-letter:uppercase">{text}</p>
    </div>
    {#if app.error}
      <button
        type="button"
        class="btn btn-icon btn-ghost size-8"
        onclick={() => (app.error = null)}
        aria-label={t("error.dismiss")}
      >
        <X size={16} strokeWidth={2} aria-hidden="true" />
      </button>
    {/if}
  </div>
{/if}
