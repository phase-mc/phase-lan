<script lang="ts">
  // The "Settings" tab is the last: how the Phase window itself looks and what language it uses.
  // "Personalization": window theme and background (built-in pictures, your own from disk, no picture).
  // "Language": Russian or English. Room styling for players is the neighboring tab.
  import Check from "@lucide/svelte/icons/check";
  import Globe from "@lucide/svelte/icons/globe";
  import ImagePlus from "@lucide/svelte/icons/image-plus";
  import Moon from "@lucide/svelte/icons/moon";
  import Paintbrush from "@lucide/svelte/icons/paintbrush";
  import Sun from "@lucide/svelte/icons/sun";
  import {
    background,
    BUILTIN,
    CUSTOM_BACKGROUND,
    NO_BACKGROUND,
  } from "../lib/background.svelte";
  import { LANGUAGES, language, t } from "../lib/i18n/index.svelte";
  import { settings } from "../lib/settings.svelte";

  let picker = $state<HTMLInputElement>();

  function onFile(event: Event & { currentTarget: HTMLInputElement }) {
    const file = event.currentTarget.files?.[0];
    event.currentTarget.value = "";
    if (file) void background.upload(file);
  }
</script>

<section class="page flex flex-col gap-6" aria-labelledby="settings-title">
  <header>
    <h1 id="settings-title" class="page-title">{t("nav.settings")}</h1>
    <p class="page-lead">{t("settings.lead")}</p>
  </header>

  <!-- Personalization: window theme and background. -->
  <section class="card p-6" aria-labelledby="look-title">
    <div class="flex items-center gap-3">
      <span class="icon-tile"><Paintbrush size={20} strokeWidth={2} aria-hidden="true" /></span>
      <div class="min-w-0 grow">
        <h2 id="look-title" class="section-title">{t("settings.personal")}</h2>
        <p class="text-[13px] text-fg-2">{t("settings.personalSub")}</p>
      </div>
    </div>

    <div class="mt-5 flex flex-wrap items-center justify-between gap-3">
      <p class="text-[14px] font-medium text-fg" id="theme-label">{t("settings.theme")}</p>
      <div class="seg-group" role="radiogroup" aria-labelledby="theme-label">
        <label class="seg">
          <input class="sr-only" type="radio" name="theme" value="dark" checked={settings.theme === "dark"} onchange={() => (settings.theme = "dark")} />
          <Moon size={15} strokeWidth={2} aria-hidden="true" />{t("settings.dark")}
        </label>
        <label class="seg">
          <input class="sr-only" type="radio" name="theme" value="light" checked={settings.theme === "light"} onchange={() => (settings.theme = "light")} />
          <Sun size={15} strokeWidth={2} aria-hidden="true" />{t("settings.light")}
        </label>
      </div>
    </div>

    <div class="mt-6 flex flex-wrap items-end justify-between gap-3">
      <div>
        <p class="text-[14px] font-medium text-fg" id="bg-label">{t("bg.title")}</p>
        <p class="mt-0.5 text-[13px] text-fg-2">{t("bg.sub")}</p>
      </div>
      {#if !background.isDefault}
        <button type="button" class="link text-[13px]" onclick={() => background.reset()} data-bg-reset>{t("bg.reset")}</button>
      {/if}
    </div>

    <div class="bg-grid mt-3" role="radiogroup" aria-labelledby="bg-label" data-bg-grid>
      {#each BUILTIN as item (item.id)}
        {@render tile(item.id, t(item.name), item.src)}
      {/each}
      {#if background.customUrl}
        {@render tile(CUSTOM_BACKGROUND, t("bg.custom"), background.customUrl)}
      {/if}
      {@render tile(NO_BACKGROUND, t("bg.none"), null)}
      <button
        type="button"
        class="bg-tile bg-add"
        onclick={() => picker?.click()}
        disabled={background.busy}
        aria-busy={background.busy}
        data-bg-add
      >
        <ImagePlus size={20} strokeWidth={2} aria-hidden="true" />
        <span class="text-[13px] font-semibold">{background.busy ? t("bg.saving") : background.customUrl ? t("bg.replace") : t("bg.add")}</span>
      </button>
    </div>
    <input bind:this={picker} class="sr-only" type="file" accept="image/png,image/jpeg,image/webp" tabindex="-1" aria-hidden="true" onchange={onFile} />

    <div class="mt-3 flex flex-wrap items-center justify-between gap-3 text-[12px] text-fg-3">
      <p>{t("bg.hint")}</p>
      {#if background.customUrl}
        <button type="button" class="link text-[12px]" onclick={() => background.removeCustom()} data-bg-delete>{t("bg.deleteCustom")}</button>
      {/if}
    </div>
    {#if background.error}
      <p class="error-text" role="alert">{background.error}</p>
    {/if}
  </section>

  <!-- Window language. -->
  <section class="card p-6" aria-labelledby="lang-title">
    <div class="flex items-center gap-3">
      <span class="icon-tile"><Globe size={20} strokeWidth={2} aria-hidden="true" /></span>
      <div class="min-w-0 grow">
        <h2 id="lang-title" class="section-title">{t("profile.language")}</h2>
        <p class="text-[13px] text-fg-2">{t("settings.languageSub")}</p>
      </div>
      <div class="seg-group shrink-0" role="radiogroup" aria-labelledby="lang-title">
        {#each LANGUAGES as option (option.code)}
          <label class="seg" lang={option.code}>
            <input
              class="sr-only"
              type="radio"
              name="language"
              value={option.code}
              checked={language.current === option.code}
              onchange={() => settings.selectLanguage(option.code)}
              data-language={option.code}
            />
            {option.native}
          </label>
        {/each}
      </div>
    </div>
  </section>
</section>

{#snippet tile(id: string, name: string, src: string | null)}
  {@const selected = background.choice === id}
  <button
    type="button"
    role="radio"
    class="bg-tile"
    aria-checked={selected}
    onclick={() => background.select(id)}
    data-bg={id}
  >
    {#if src}
      <img class="bg-img" {src} alt="" loading="lazy" decoding="async" />
    {:else}
      <span class="bg-solid" aria-hidden="true"></span>
    {/if}
    <span class="bg-name">{name}</span>
    {#if selected}<span class="bg-check" aria-hidden="true"><Check size={13} strokeWidth={2.6} /></span>{/if}
  </button>
{/snippet}

<style>
  .bg-grid {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 10px;
  }

  .bg-tile {
    position: relative;
    display: flex;
    aspect-ratio: 16 / 9;
    overflow: hidden;
    align-items: flex-end;
    border: 1px solid var(--card-border);
    border-radius: 12px;
    background: var(--bg-input);
    text-align: left;
    transition: border-color 200ms var(--ease-ui);
  }

  .bg-tile:hover:not(:disabled) {
    border-color: var(--card-border-hover);
  }

  .bg-tile[aria-checked="true"] {
    border-color: var(--primary);
    box-shadow: inset 0 0 0 1px var(--primary);
  }

  .bg-img {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: cover;
    transition: transform 450ms var(--ease-ui);
  }

  .bg-tile:hover .bg-img {
    transform: scale(1.04);
  }

  /* "No picture" — a sample of the theme's solid canvas. */
  .bg-solid {
    position: absolute;
    inset: 0;
    background: var(--bg-app);
  }

  /* Caption on the picture: white on a dark gradient (contrast doesn't depend on the picture). */
  .bg-name {
    position: relative;
    width: 100%;
    background: linear-gradient(0deg, rgba(4, 8, 20, 0.82) 0%, rgba(4, 8, 20, 0) 100%);
    padding: 18px 10px 7px;
    color: #ffffff;
    font-size: 12px;
    font-weight: 600;
  }

  .bg-tile:has(.bg-solid) .bg-name {
    background: none;
    color: var(--text-1);
  }

  .bg-check {
    position: absolute;
    top: 7px;
    right: 7px;
    display: grid;
    width: 20px;
    height: 20px;
    place-items: center;
    border-radius: 9999px;
    background: var(--primary);
    color: #ffffff;
  }

  .bg-add {
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border-style: dashed;
    border-color: var(--border-strong);
    background: transparent;
    color: var(--text-2);
  }

  .bg-add:hover:not(:disabled) {
    color: var(--text-1);
  }

  @media (max-width: 760px) {
    .bg-grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .bg-img {
      transition: none;
    }

    .bg-tile:hover .bg-img {
      transform: none;
    }
  }
</style>
