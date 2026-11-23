<script lang="ts">
  // The "guest mods" card: comparison with the host's mods, the "Sync",
  // "Skip" and "Restore my mods" buttons, download progress, a game restart hint and
  // the auto-mode toggle. Lives on the "Join" tab (when the guest is already in the room) and
  // on the "Mods" tab (visible there without a connection too — to restore mods from the backup).
  import Download from "@lucide/svelte/icons/download";
  import Puzzle from "@lucide/svelte/icons/puzzle";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import RotateCcw from "@lucide/svelte/icons/rotate-ccw";
  import SkipForward from "@lucide/svelte/icons/skip-forward";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import { app } from "../lib/app.svelte";
  import { formatSize, modsWord, type ModSyncState } from "../lib/core";
  import { t, type Key } from "../lib/i18n/index.svelte";
  import { dirSourceLabel, mods } from "../lib/mods.svelte";
  import { nav } from "../lib/nav.svelte";
  import { settings } from "../lib/settings.svelte";
  import Orb from "./Orb.svelte";

  interface Props {
    /** Where the card sits: on "Join" it leads to the settings on the "Mods" tab. */
    context: "join" | "mods";
  }

  let { context }: Props = $props();
  const uid = $props.id();

  const sync = $derived(app.modSync);
  const stage = $derived(sync?.state ?? null);
  const busy = $derived(stage ==="checking" || stage ==="syncing");
  const progress = $derived(sync?.progress ?? null);
  /** Download fraction from 0 to 1. */
  const fraction = $derived(
    progress && progress.bytes_total > 0
      ? Math.min(1, progress.bytes_done / progress.bytes_total)
      : progress
        ? progress.file_index / Math.max(1, progress.files_total)
        : 0,
  );

  const BADGE: Record<ModSyncState, [string, Key]> = {
    not_required: ["", "sync.badgeNotRequired"],
    checking: ["badge-blue", "sync.badgeChecking"],
    needs_sync: ["badge-warn", "sync.badgeNeeds"],
    syncing: ["badge-blue", "sync.badgeSyncing"],
    done: ["badge-green", "sync.badgeDone"],
    skipped: ["", "sync.badgeSkipped"],
    failed: ["badge-red", "sync.badgeFailed"],
  };

  /** Whether own mods can be restored: during a connection the session knows, without one — the folder. */
  const canRestore = $derived(sync ? sync.can_restore : (mods.overview?.can_restore ?? false));
  const backupFiles = $derived(sync ? sync.backup_files : (mods.overview?.backup_files ?? 0));

  function restore() {
    if (sync) void app.modAction("restore");
    else void mods.restore();
  }

  // Sync or restore finished — the folder changed: re-read it.
  let seen: ModSyncState | null = null;
  $effect(() => {
    const now = stage;
    if (seen !== now && (now === "done" || now === "skipped" || now === null)) void mods.refresh();
    seen = now;
  });
</script>

{#snippet syncButton(primary: boolean, label: string)}
  <!-- The screen's main action while the guest decides about mods: a white capsule with an orb. -->
  {#if primary}
    <button type="button" class="btn-pill" onclick={() => app.modAction("sync")} data-mods-sync>
      {label}
      <Orb>
        <Download size={17} strokeWidth={2.4} />
      </Orb>
    </button>
  {:else}
    <button type="button" class="btn-ghost-pill" onclick={() => app.modAction("sync")} data-mods-sync>
      <Download size={16} strokeWidth={2.2} aria-hidden="true" />
      {label}
    </button>
  {/if}
{/snippet}

{#snippet skipButton()}
  <button type="button" class="btn-ghost-pill" onclick={() => app.modAction("skip")} data-mods-skip>
    <SkipForward size={16} strokeWidth={2} aria-hidden="true" />
    {t("sync.skip")}
  </button>
{/snippet}

<section class="card p-6" aria-labelledby="{uid}-title" aria-live="polite" data-mod-sync={stage ?? "idle"}>
  <header class="flex flex-wrap items-center justify-between gap-3">
    <div class="flex items-center gap-3">
      <span class="icon-tile"><Puzzle size={20} strokeWidth={2} aria-hidden="true" /></span>
      <div>
        <h2 id="{uid}-title" class="section-title">
          {context === "mods" ? t("sync.titleGuest") : t("sync.titleRoom")}
        </h2>
        <p class="text-[13px] text-fg-2">
          {#if sync && sync.required > 0}
            {t("sync.hostHas", { mods: modsWord(sync.required), size: formatSize(sync.required_bytes) })}
          {:else}
            {t("sync.sameMods")}
          {/if}
        </p>
      </div>
    </div>
    {#if stage}
      <span class="badge {BADGE[stage][0]}">
        {#if busy}<span class="dot dot-busy dot-pulse" aria-hidden="true"></span>{/if}
        {t(BADGE[stage][1])}
      </span>
    {/if}
  </header>

  {#if !sync}
    <p class="mt-4 text-[14px] text-fg-2">
      {app.role === "host"
        ? t("sync.youAreHost")
        : t("sync.willCheck")}
    </p>
  {:else if stage ==="checking"}
    <p class="mt-4 text-[16px] font-semibold text-fg">{t("sync.checking")}</p>
    <p class="mt-1 text-[13px] text-fg-2">{t("sync.checkingSub")}</p>
  {:else if stage ==="needs_sync" || stage ==="skipped"}
    <p class="mt-4 text-[20px] leading-tight font-semibold text-fg tabular-nums">
      {#if stage ==="skipped"}
        {t("sync.enteringWithout")}
      {:else if sync.missing > 0}
        {t("sync.missing", { mods: modsWord(sync.missing), size: formatSize(sync.missing_bytes) })}
      {:else}
        {t("sync.extraCount", { mods: modsWord(sync.extra) })}
      {/if}
    </p>
    <p class="mt-1 max-w-[70ch] text-[13px] text-fg-2">
      {#if stage ==="skipped"}
        {t("sync.worldMayRefuse")}
        {#if sync.missing > 0}{t("sync.missing", { mods: modsWord(sync.missing), size: formatSize(sync.missing_bytes) })}.{/if}
      {/if}
      {#if sync.extra > 0}
        {stage === "skipped" ? t("sync.extraBeforeSkipped", { mods: modsWord(sync.extra) }) : t("sync.extraBefore", { mods: modsWord(sync.extra) })}
        <span class="font-mono text-[12px] whitespace-nowrap text-fg">mods\.phase-backup</span>
        {t("sync.extraAfter")}
      {:else if stage ==="needs_sync"}
        {t("sync.noneExtra")}
      {/if}
    </p>
    {#if sync.missing_names.length + sync.extra_names.length > 0}
      <details class="mt-2 text-[13px]">
        <summary class="link inline-block">{t("sync.showFiles")}</summary>
        <div class="mt-2 grid max-h-44 grid-cols-2 gap-x-6 gap-y-3 overflow-y-auto rounded-[10px] border border-line bg-input p-3">
          {#if sync.missing_names.length > 0}
            <div class="min-w-0">
              <p class="mb-1 text-[12px] font-medium text-fg-2">{t("sync.download")}</p>
              {#each sync.missing_names as name (name)}
                <p class="truncate font-mono text-[12px] text-fg">{name}</p>
              {/each}
            </div>
          {/if}
          {#if sync.extra_names.length > 0}
            <div class="min-w-0">
              <p class="mb-1 text-[12px] font-medium text-fg-2">{t("sync.toBackup")}</p>
              {#each sync.extra_names as name (name)}
                <p class="truncate font-mono text-[12px] text-fg">{name}</p>
              {/each}
            </div>
          {/if}
        </div>
      </details>
    {/if}
    <div class="mt-4 flex flex-wrap gap-3">
      {@render syncButton(stage ==="needs_sync", t("sync.syncMods"))}
      {#if stage ==="needs_sync"}{@render skipButton()}{/if}
    </div>
  {:else if stage ==="syncing"}
    <p class="mt-4 flex min-w-0 items-baseline gap-2 text-[13px] text-fg-2">
      {#if progress}
        <span class="shrink-0">{t("sync.downloading")}</span>
        <span class="truncate font-mono text-[12px] text-fg">{progress.file}</span>
      {:else}
        {t("sync.movingExtra")}
      {/if}
    </p>
    <div
      class="meter mt-2.5"
      role="progressbar"
      aria-label={t("sync.downloadBar")}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(fraction * 100)}
      style:--value={fraction}
    >
      <span></span>
    </div>
    {#if progress}
      <p class="mt-2.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[14px] font-semibold text-fg tabular-nums" data-mods-progress>
        <span>{t("sync.fileProgress", { i: progress.file_index, n: progress.files_total })}</span>
        <span class="text-fg-3" aria-hidden="true">·</span>
        <span>{t("sync.ofSize", { done: formatSize(progress.bytes_done), total: formatSize(progress.bytes_total) })}</span>
        <span class="text-fg-3" aria-hidden="true">·</span>
        <span class="badge {progress.source === 'modrinth' ? 'badge-green' : 'badge-blue'}">
          {progress.source === "modrinth" ? "Modrinth" : "P2P"}
        </span>
      </p>
    {/if}
    <div class="mt-4 flex flex-wrap gap-3">{@render skipButton()}</div>
  {:else if stage ==="done"}
    <p class="mt-4 text-[20px] leading-tight font-semibold text-fg">{t("sync.matches")}</p>
    <p class="mt-1 text-[13px] text-fg-2 tabular-nums">
      {#if sync.from_modrinth + sync.from_host > 0}
        {t("sync.downloaded", { modrinth: sync.from_modrinth, host: sync.from_host })}
      {:else}
        {t("sync.nothingDownloaded")}
      {/if}
      {#if sync.backed_up > 0}{t("sync.backedUp", { n: sync.backed_up })}{/if}
    </p>
    {#if sync.needs_mc_restart}
      <p class="note mt-3" role="status" data-mods-restart>
        <TriangleAlert class="mt-px shrink-0" size={16} strokeWidth={2} aria-hidden="true" />
        {t("sync.restartMinecraft")}
      </p>
    {/if}
  {:else if stage ==="failed"}
    <p class="mt-4 text-[16px] font-semibold text-fg">{t("sync.failed")}</p>
    <div class="mt-4 flex flex-wrap gap-3">
      {@render syncButton(true, t("sync.retry"))}
      {@render skipButton()}
    </div>
  {:else}
    <p class="mt-4 text-[20px] leading-tight font-semibold text-fg">{t("sync.notRequired")}</p>
    <p class="mt-1 text-[13px] text-fg-2">
      {sync.host_shares
        ? t("sync.hostNotSharing")
        : t("sync.hostOld")}
    </p>
  {/if}

  {#if sync?.error}
    <p class="note {stage ==='failed' ? 'note-danger' : ''} mt-3" role="alert">
      <TriangleAlert class="mt-px shrink-0" size={16} strokeWidth={2} aria-hidden="true" />
      <span class="min-w-0 wrap-anywhere">{sync.error}</span>
    </p>
  {/if}
  {#if !sync && mods.notice}
    <p class="note note-ok mt-3" role="status">{mods.notice}</p>
  {/if}

  {#if (canRestore && !busy) || (sync?.host_shares && !busy && stage !== "needs_sync")}
    <div class="mt-4 flex flex-wrap gap-3">
      {#if canRestore && !busy}
        <button type="button" class="btn" onclick={restore} disabled={mods.restoring} data-mods-restore>
          <RotateCcw size={16} strokeWidth={2} aria-hidden="true" />
          {t("sync.restore")}
          {#if backupFiles > 0}<span class="seg-count">{backupFiles}</span>{/if}
        </button>
      {/if}
      {#if sync?.host_shares && !busy && stage !== "needs_sync"}
        <button type="button" class="btn btn-ghost" onclick={() => app.modAction("recheck")}>
          <RefreshCw size={16} strokeWidth={2} aria-hidden="true" />
          {t("sync.recheck")}
        </button>
      {/if}
    </div>
  {/if}

  <footer class="mt-4 flex flex-wrap items-center justify-between gap-x-6 gap-y-3 border-t border-line pt-4">
    <label class="flex cursor-pointer items-center gap-3 text-[13px] text-fg">
      <button
        type="button"
        role="switch"
        class="switch"
        aria-checked={settings.modsAuto}
        aria-label={t("sync.auto")}
        onclick={() => (settings.modsAuto = !settings.modsAuto)}
      ></button>
      <span>
        {t("sync.auto")}
        <span class="text-fg-2">
          {sync && sync.auto !== settings.modsAuto ? t("sync.autoNext") : t("sync.autoOnJoin")}
        </span>
      </span>
    </label>
    {#if sync?.dir}
      <p class="flex min-w-0 items-center gap-2 text-[12px] text-fg-2">
        <span class="truncate font-mono text-fg">{sync.dir}</span>
        {#if sync.dir_source}<span class="badge shrink-0">{dirSourceLabel(sync.dir_source)}</span>{/if}
        {#if context === "join"}
          <button type="button" class="link shrink-0" onclick={() => nav.go("mods")}>{t("sync.configure")}</button>
        {/if}
      </p>
    {:else if context === "join"}
      <button type="button" class="link text-[13px]" onclick={() => nav.go("mods")}>{t("sync.modsFolder")}</button>
    {/if}
  </footer>
</section>
