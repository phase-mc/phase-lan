<script lang="ts">
  // The "Journal" tab: session events as plain lines and the core's system log
  // (what tracing writes: tunnel, relay, errors). New entries on top.
  import ScrollText from "@lucide/svelte/icons/scroll-text";
  import { app } from "../lib/app.svelte";
  import type { LogLine } from "../lib/core";
  import { fmtTime, t, type Key } from "../lib/i18n/index.svelte";
  import type { Tone } from "../lib/journal";

  type Source = "events" | "system";
  type Filter = "all" | "important";
  let source = $state<Source>("events");
  let filter = $state<Filter>("all");
  const important = $derived(filter === "important");

  const TAG: Record<Tone, [Key, string]> = {
    info: ["journalTab.tagInfo", ""],
    ok: ["journalTab.tagOk", "badge-green"],
    warn: ["journalTab.tagWarn", "badge-warn"],
    error: ["journalTab.tagError", "badge-red"],
  };
  const LEVEL: Record<LogLine["level"], [string, string]> = {
    error: ["Error", "badge-red"],
    warn: ["Warn", "badge-warn"],
    info: ["Info", "badge-blue"],
    debug: ["Debug", ""],
    trace: ["Trace", ""],
  };

  const events = $derived(
    important ? app.journal.filter((e) => e.tone === "warn" || e.tone === "error") : app.journal,
  );
  const logs = $derived(
    important ? app.logs.filter((l) => l.level === "warn" || l.level === "error") : app.logs,
  );

  const clock = (at: Date) => fmtTime(at);
  /** `phase_tunnel::guest` → `tunnel::guest`. */
  const module = (target: string) => target.replace(/^phase_/, "");
</script>

{#snippet empty(text: string)}
  <div class="flex flex-col items-center gap-3 py-14 text-center">
    <span class="grid size-12 place-items-center rounded-full bg-elevated text-fg-2">
      <ScrollText size={22} strokeWidth={2} aria-hidden="true" />
    </span>
    <p class="max-w-[40ch] text-[14px] text-fg-2">{text}</p>
  </div>
{/snippet}

<section class="page flex min-h-0 grow flex-col gap-5" aria-labelledby="journal-title">
  <header>
    <h1 id="journal-title" class="page-title">{t("nav.journal")}</h1>
    <p class="page-lead">{t("journalTab.lead")}</p>
  </header>

  <div class="card flex min-h-[320px] grow flex-col">
    <div class="flex flex-wrap items-center justify-between gap-3 border-b border-line px-4 py-3">
      <div class="seg-group" role="radiogroup" aria-label={t("journalTab.source")}>
        <label class="seg">
          <input type="radio" class="sr-only" bind:group={source} value="events" />
          {t("journalTab.events")} <span class="seg-count">{app.journal.length}</span>
        </label>
        <label class="seg">
          <input type="radio" class="sr-only" bind:group={source} value="system" />
          {t("journalTab.system")} <span class="seg-count">{app.logs.length}</span>
        </label>
      </div>
      <div class="seg-group" role="radiogroup" aria-label={t("journalTab.filter")}>
        <label class="seg">
          <input type="radio" class="sr-only" bind:group={filter} value="all" />
          {t("journalTab.all")}
        </label>
        <label class="seg">
          <input type="radio" class="sr-only" bind:group={filter} value="important" />
          {t("journalTab.important")}
        </label>
      </div>
    </div>

    <div class="min-h-0 grow overflow-y-auto p-2">
      {#if source === "events"}
        {#if events.length === 0}
          {@render empty(important ? t("journalTab.noImportant") : t("journalTab.noEvents"))}
        {:else}
          <ol aria-live="polite">
            {#each events as entry (entry.id)}
              <li class="grid grid-cols-[76px_72px_1fr] items-baseline gap-x-3 rounded-[10px] px-3 py-2 transition-colors duration-200 ease-ui hover:bg-card-hover">
                <time class="font-mono text-[12px] text-fg-3 tabular-nums" datetime={entry.at.toISOString()}>{clock(entry.at)}</time>
                <span><span class="badge {TAG[entry.tone][1]}">{t(TAG[entry.tone][0])}</span></span>
                <span class="min-w-0 text-[14px] wrap-anywhere text-fg">{entry.text}</span>
              </li>
            {/each}
          </ol>
        {/if}
      {:else if logs.length === 0}
        {@render empty(important ? t("journalTab.noWarnings") : t("journalTab.noLogs"))}
      {:else}
        <ol class="text-[12px] leading-[1.5]">
          {#each logs as line, i (`${line.unix_ms}-${i}`)}
            <li class="grid grid-cols-[76px_60px_160px_1fr] items-baseline gap-x-3 rounded-[10px] px-3 py-1.5 transition-colors duration-200 ease-ui hover:bg-card-hover">
              <time class="font-mono text-fg-3 tabular-nums" datetime={new Date(line.unix_ms).toISOString()}>
                {clock(new Date(line.unix_ms))}
              </time>
              <span><span class="badge {LEVEL[line.level][1]}">{LEVEL[line.level][0]}</span></span>
              <span class="truncate font-mono text-fg-2">{module(line.target)}</span>
              <span class="min-w-0 font-mono wrap-anywhere text-fg">{line.message}</span>
            </li>
          {/each}
        </ol>
      {/if}
    </div>
  </div>
</section>
