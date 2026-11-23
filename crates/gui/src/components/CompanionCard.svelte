<!--
  Phase Companion: a mod for the host that enables the Tab header, nick colors, more than 8 slots (up to 50)
  and joining without an account check. Installed with one button: Phase downloads the mod from releases,
  verifies the checksum and puts it into the mods folder. Guests don't need the mod.
-->
<script lang="ts">
  import { app } from "../lib/app.svelte";
  import { core, type CompanionOutcome } from "../lib/core";
  import { t } from "../lib/i18n/index.svelte";
  import { settings } from "../lib/settings.svelte";

  let busy = $state(false);
  let outcome = $state<CompanionOutcome | null>(null);
  let failure = $state<string | null>(null);

  const gameVersion = $derived(app.status?.host?.game_version ?? null);

  async function install() {
    busy = true;
    failure = null;
    try {
      outcome = await core.installCompanion(settings.modsDir, gameVersion);
    } catch (e) {
      outcome = null;
      failure = String(e);
    } finally {
      busy = false;
    }
  }

  const message = $derived.by(() => {
    if (failure) return { tone: "bad", text: failure };
    switch (outcome?.kind) {
      case "installed":
        return outcome.restart
          ? { tone: "ok", text: t("companion.doneRestart") }
          : { tone: "ok", text: t("companion.doneNext") };
      case "up_to_date":
        return { tone: "ok", text: t("companion.upToDate") };
      case "no_build":
        return { tone: "warn", text: t("companion.noBuild") };
      case "unknown_loader":
        return { tone: "warn", text: t("companion.unknownLoader") };
      case "unknown_game":
        return { tone: "warn", text: t("companion.unknownGame") };
      case "no_mods_folder":
        return { tone: "warn", text: t("companion.noModsFolder") };
      default:
        return null;
    }
  });
</script>

<div class="flex items-center justify-between gap-3">
  <div class="min-w-0">
    <p class="text-[14px] font-medium text-fg-1">Phase Companion</p>
    <p class="mt-0.5 text-[12px] text-fg-3">
      {t("companion.text")}
    </p>
  </div>
  <button type="button" class="btn-ghost-pill shrink-0" disabled={busy} onclick={install} data-companion-install>
    {busy ? t("companion.installing") : t("companion.install")}
  </button>
</div>
{#if message}
  <p
    class="mt-2 text-[12px] {message.tone === 'ok' ? 'text-live-text' : message.tone === 'bad' ? 'text-danger-text' : 'text-fg-2'}"
    role="status"
    aria-live="polite"
    data-companion-status
  >
    {message.text}
  </p>
{/if}
