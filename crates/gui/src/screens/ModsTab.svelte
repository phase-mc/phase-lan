<script lang="ts">
  // The "Mods" tab. Three cards top to bottom: which mods folder is used (and picking one
  // manually), what the host distributes to guests (a toggle and checkboxes), guest-side sync
  // (the same card as on the "Join" tab). Settings save themselves and
  // take effect at once: an open room rebuilds the distribution without a restart.
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import Search from "@lucide/svelte/icons/search";
  import Share2 from "@lucide/svelte/icons/share-2";
  import TriangleAlert from "@lucide/svelte/icons/triangle-alert";
  import ClientBanner from "../components/ClientBanner.svelte";
  import ModSyncCard from "../components/ModSyncCard.svelte";
  import { app } from "../lib/app.svelte";
  import { formatSize, modsWord } from "../lib/core";
  import { t } from "../lib/i18n/index.svelte";
  import { dirSourceLabel, mods } from "../lib/mods.svelte";
  import { settings } from "../lib/settings.svelte";

  const overview = $derived(mods.overview);
  const files = $derived(overview?.files ?? []);
  const shared = $derived(mods.shared);
  /** Distribution of the running room (what guests get right now). */
  const live = $derived(app.role === "host" ? (app.status?.host?.mods ?? null) : null);

  let filter = $state("");
  const shown = $derived.by(() => {
    const query = filter.trim().toLowerCase();
    return query ? files.filter((f) => f.file_name.toLowerCase().includes(query)) : files;
  });

  // The folder changed (manually or via the button) — re-read it; the first time — when the tab opens.
  $effect(() => {
    void settings.modsDir;
    const timer = setTimeout(() => void mods.refresh(), 250);
    return () => clearTimeout(timer);
  });
</script>

<section class="page flex flex-col gap-6" aria-labelledby="mods-title">
  <header class="flex items-end justify-between gap-6">
    <div>
      <h1 id="mods-title" class="page-title">{t("nav.mods")}</h1>
      <p class="page-lead">
        {t("modsTab.lead")}
      </p>
    </div>
    {#if live}
      <span class="pill {live.sharing && live.files > 0 ? 'pill-live' : ''}">
        <span class="dot {live.sharing && live.files > 0 ? 'dot-live' : ''}" aria-hidden="true"></span>
        {live.sharing && live.files > 0 ? t("modsTab.roomShares", { mods: modsWord(live.files) }) : t("modsTab.roomNoShare")}
      </span>
    {:else if overview?.game_running}
      <span class="pill"><span class="dot dot-live" aria-hidden="true"></span>{t("modsTab.gameRunning")}</span>
    {/if}
  </header>

  <ClientBanner />

  <!-- The mods folder. -->
  <section class="card p-6" aria-labelledby="dir-title">
    <div class="flex items-center gap-3">
      <span class="icon-tile"><FolderOpen size={20} strokeWidth={2} aria-hidden="true" /></span>
      <div class="min-w-0 grow">
        <h2 id="dir-title" class="section-title">{t("modsTab.folder")}</h2>
        <p class="text-[13px] text-fg-2">
          {t("modsTab.folderOrder")} <span class="font-mono text-[12px]">.minecraft\mods</span>.
        </p>
      </div>
      <button
        type="button"
        class="btn btn-icon btn-ghost shrink-0"
        aria-label={t("modsTab.reread")}
        onclick={() => mods.refresh()}
        disabled={mods.loading}
      >
        <RefreshCw size={16} strokeWidth={2} aria-hidden="true" />
      </button>
    </div>

    <p class="mt-4 flex min-w-0 items-center gap-2.5 rounded-[10px] border border-line bg-input px-3.5 py-2.5" data-mods-dir>
      {#if overview?.dir}
        <span class="dot {overview.exists ? 'dot-live' : 'dot-warn'}" aria-hidden="true"></span>
        <span class="min-w-0 truncate font-mono text-[13px] text-fg">{overview.dir}</span>
        {#if overview.source}<span class="badge shrink-0">{dirSourceLabel(overview.source)}</span>{/if}
        <span class="ml-auto shrink-0 text-[12px] text-fg-2 tabular-nums">
          {overview.exists ? `${modsWord(files.length)} · ${formatSize(overview.total_size)}` : t("modsTab.noFolderYet")}
        </span>
      {:else if mods.loading || !overview}
        <span class="dot dot-busy dot-pulse" aria-hidden="true"></span>
        <span class="text-[13px] text-fg-2">{t("modsTab.looking")}</span>
      {:else}
        <span class="dot dot-warn" aria-hidden="true"></span>
        <span class="text-[13px] text-fg-2">{t("modsTab.notFound")}</span>
      {/if}
    </p>

    <div class="mt-4 flex flex-wrap items-end gap-3">
      <div class="min-w-[240px] grow">
        <label class="label" for="mods-dir">{t("modsTab.customFolder")}</label>
        <input
          id="mods-dir"
          class="field h-10 font-mono text-[13px]"
          placeholder="C:\Games\MyPack\mods"
          spellcheck="false"
          autocomplete="off"
          bind:value={settings.modsDir}
        />
      </div>
      <button type="button" class="btn" onclick={() => mods.pickDir()}>
        <FolderOpen size={16} strokeWidth={2} aria-hidden="true" />
        {t("modsTab.pick")}
      </button>
      {#if settings.modsDir.trim()}
        <button type="button" class="btn btn-ghost" onclick={() => (settings.modsDir = "")}>{t("modsTab.reset")}</button>
      {/if}
    </div>
    {#if overview?.source === "running_game" && settings.modsDir.trim()}
      <p class="mt-2 text-[12px] text-fg-3">
        {t("modsTab.runningNote")}
      </p>
    {/if}
    {#if mods.error}
      <p class="error-text" role="alert">{mods.error}</p>
    {/if}
  </section>

  <!-- Host distribution. -->
  <section class="card p-6" aria-labelledby="share-title">
    <div class="flex items-center gap-3">
      <span class="icon-tile"><Share2 size={20} strokeWidth={2} aria-hidden="true" /></span>
      <div class="min-w-0 grow">
        <h2 id="share-title" class="section-title">{t("modsTab.share")}</h2>
        <p class="text-[13px] text-fg-2">{t("modsTab.shareSub")}</p>
      </div>
      <label class="flex shrink-0 cursor-pointer items-center gap-3 text-[13px] font-medium text-fg">
        <span>{t("modsTab.shareToggle")}</span>
        <button
          type="button"
          role="switch"
          class="switch"
          aria-checked={settings.modsShare}
          aria-label={t("modsTab.shareToggle")}
          onclick={() => (settings.modsShare = !settings.modsShare)}
          data-mods-share
        ></button>
        <span class="w-9 text-fg-2">{settings.modsShare ? t("common.on") : t("common.off")}</span>
      </label>
    </div>

    <div class="mt-4 transition-opacity duration-200 ease-ui {settings.modsShare ? '' : 'opacity-55'}">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <p class="text-[14px] text-fg tabular-nums" data-mods-summary>
          {#if !settings.modsShare}
            {t("modsTab.sharingOff")}
          {:else if files.length === 0}
            {t("modsTab.nothingToShare")}
          {:else}
            {t("modsTab.guestsGet")} <span class="font-semibold">{t("modsTab.ofN", { n: shared.files, total: files.length })}</span>
            <span class="text-fg-2">· {formatSize(shared.size)}</span>
          {/if}
        </p>
        {#if files.length > 0}
          <div class="flex items-center gap-4 text-[13px]">
            <button type="button" class="link" onclick={() => mods.setAll(true)} disabled={!settings.modsShare}>{t("modsTab.markAll")}</button>
            <button type="button" class="link" onclick={() => mods.setAll(false)} disabled={!settings.modsShare}>{t("modsTab.unmarkAll")}</button>
          </div>
        {/if}
      </div>

      {#if live && live.sharing}
        <p class="mt-1 text-[13px] text-fg-2 tabular-nums">
          {t("modsTab.sentByGuests", { n: live.sent_files, size: formatSize(live.sent_bytes) })}
        </p>
      {/if}
      {#if live?.error}
        <p class="note mt-3" role="alert">
          <TriangleAlert class="mt-px shrink-0" size={16} strokeWidth={2} aria-hidden="true" />
          <span class="min-w-0 wrap-anywhere">{live.error}</span>
        </p>
      {/if}

      {#if files.length > 8}
        <div class="relative mt-3">
          <Search class="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-fg-3" size={15} strokeWidth={2} aria-hidden="true" />
          <label class="sr-only" for="mods-filter">{t("modsTab.searchLabel")}</label>
          <input
            id="mods-filter"
            class="field h-9 pl-9 text-[13px]"
            placeholder={t("modsTab.searchPlaceholder")}
            autocomplete="off"
            spellcheck="false"
            bind:value={filter}
          />
        </div>
      {/if}

      {#if files.length > 0}
        <ul class="mt-3 max-h-[300px] overflow-y-auto rounded-[12px] border border-line bg-input p-1.5" aria-label={t("modsTab.listLabel")} data-mods-list>
          {#each shown as file (file.file_name)}
            {@const on = file.shareable && !file.companion && mods.isShared(file.file_name)}
            <li>
              <label class="mod-row">
                <input
                  type="checkbox"
                  class="check"
                  checked={on}
                  disabled={!file.shareable || file.companion || !settings.modsShare}
                  onchange={(e) => mods.toggle(file.file_name, e.currentTarget.checked)}
                />
                <span class="min-w-0 grow truncate {on ? 'text-fg' : 'text-fg-3'}">{file.file_name}</span>
                {#if file.companion}
                  <span class="badge shrink-0" title={t("modsTab.companionTitle")}>{t("modsTab.hostOnly")}</span>
                {:else if !file.shareable}
                  <span class="badge badge-warn shrink-0">{t("modsTab.badName")}</span>
                {/if}
                <span class="shrink-0 font-mono text-[12px] text-fg-2 tabular-nums">{formatSize(file.size)}</span>
              </label>
            </li>
          {:else}
            <li class="px-3 py-4 text-center text-[13px] text-fg-2">{t("modsTab.nothingFound")}</li>
          {/each}
        </ul>
        <p class="mt-2.5 text-[12px] text-fg-3">
          {t("modsTab.clientOnly")}
        </p>
      {:else if mods.loading && !overview}
        <div class="mt-3 space-y-2" aria-hidden="true">
          {#each [72, 56, 64] as width (width)}
            <div class="h-10 rounded-[10px] bg-input" style:width="{width}%"></div>
          {/each}
        </div>
      {:else}
        <div class="mt-3 rounded-[12px] border border-dashed border-line-strong px-4 py-7 text-center">
          <p class="text-[14px] font-medium text-fg">{t("modsTab.empty")}</p>
          <p class="mt-1 text-[13px] text-fg-2">
            {t("modsTab.emptyHint")}
          </p>
        </div>
      {/if}
    </div>
  </section>

  <!-- Guest. -->
  <ModSyncCard context="mods" />
</section>
