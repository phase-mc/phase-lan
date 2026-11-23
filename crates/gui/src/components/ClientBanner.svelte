<script lang="ts">
  // A calm Phase Client announcement on the "Mods" tab: not a modal, closed with a cross and never
  // comes back. The Phase LAN installer doesn't install the second product — the announcement lives here.
  import X from "@lucide/svelte/icons/x";
  import { t } from "../lib/i18n/index.svelte";

  const KEY = "phase.client-banner.hidden";

  function read(): boolean {
    try {
      return localStorage.getItem(KEY) === "1";
    } catch {
      return false;
    }
  }

  let hidden = $state(read());

  function close() {
    hidden = true;
    try {
      localStorage.setItem(KEY, "1");
    } catch {
      // Without storage the banner just returns at the next launch.
    }
  }
</script>

{#if !hidden}
  <aside class="card client-banner" aria-label="Phase Client">
    <div class="min-w-0 grow">
      <p class="text-[15px] font-semibold text-fg">{t("client.title")}</p>
      <p class="mt-1 text-[13px] text-fg-2">
        {t("client.text")}
      </p>
    </div>
    <button type="button" class="icon-capsule" aria-label={t("client.dismiss")} onclick={close}>
      <X size={16} strokeWidth={2.2} aria-hidden="true" />
    </button>
  </aside>
{/if}

<style>
  .client-banner {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 16px 16px 16px 20px;
    border-color: color-mix(in srgb, #38bdf8 32%, var(--card-border));
    background:
      radial-gradient(120% 140% at 0% 0%, rgba(56, 189, 248, 0.1), transparent 60%),
      var(--bg-card);
  }
</style>
