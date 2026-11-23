<script lang="ts">
  // Profile in the top row: the nickname with an avatar and a device state dot — editable right
  // here (NicknameChip); the chevron opens the profile menu, as in game launchers: who you are
  // and the device ID (a click copies it), profiles on this PC, "New profile", quick
  // settings as tiles (theme, room styling).
  // Dot: green — the device is ready (pulses during a session), light blue — connecting,
  // red — the key couldn't be read. The nickname and profile don't change during a session.
  import Check from "@lucide/svelte/icons/check";
  import ChevronDown from "@lucide/svelte/icons/chevron-down";
  import Copy from "@lucide/svelte/icons/copy";
  import Globe from "@lucide/svelte/icons/globe";
  import Image from "@lucide/svelte/icons/image";
  import Moon from "@lucide/svelte/icons/moon";
  import Palette from "@lucide/svelte/icons/palette";
  import Plus from "@lucide/svelte/icons/plus";
  import { onMount } from "svelte";
  import { app } from "../lib/app.svelte";
  import { copyText } from "../lib/clipboard";
  import { shortId } from "../lib/core";
  import { LANGUAGES, language, t } from "../lib/i18n/index.svelte";
  import { nav, type Tab } from "../lib/nav.svelte";
  import { profiles } from "../lib/profiles.svelte";
  import { settings } from "../lib/settings.svelte";
  import Avatar from "./Avatar.svelte";
  import Modal from "./Modal.svelte";
  import NicknameChip from "./NicknameChip.svelte";
  import Popover from "./Popover.svelte";

  onMount(() => void profiles.load());

  const locked = $derived(app.active || app.starting !== null);
  const id = $derived(profiles.current?.device_id ?? null);
  const running = $derived(app.status?.phase === "running" && app.active);
  const dot = $derived(
    profiles.error || profiles.current?.error
      ? "dot-danger"
      : running
        ? "dot-live dot-pulse"
        : app.active || app.starting
          ? "dot-busy dot-pulse"
          : "dot-live",
  );
  const dotLabel = $derived(
    profiles.error || profiles.current?.error
      ? t("profile.dotKey")
      : running
        ? t("profile.dotRunning")
        : app.active || app.starting
          ? t("profile.dotConnecting")
          : t("profile.dotReady"),
  );

  let copied = $state(false);
  let timer: ReturnType<typeof setTimeout> | undefined;

  async function copyId() {
    if (!id) return;
    copied = await copyText(id);
    clearTimeout(timer);
    timer = setTimeout(() => (copied = false), 1600);
  }

  // ---------------------------------------------------------------- profile menu
  let menuOpen = $state(false);

  function closeMenu() {
    document.getElementById("profile-pop")?.hidePopover();
  }

  function choose(name: string) {
    if (locked) return;
    settings.profile = name;
    closeMenu();
  }

  function showTab(tab: Tab) {
    closeMenu();
    nav.go(tab);
  }

  // ---------------------------------------------------------------- new profile
  let creating = $state(false);
  let newName = $state("");
  let newError = $state<string | null>(null);

  function openCreate() {
    closeMenu();
    newName = "";
    newError = null;
    creating = true;
  }

  async function create(event: SubmitEvent) {
    event.preventDefault();
    newError = await profiles.create(newName);
    if (!newError) creating = false;
  }
</script>

<div class="capsule">
  <NicknameChip {locked} {dot} {dotLabel} />
  <button
    type="button"
    class="capsule-btn w-9 justify-center px-0 text-fg-2"
    popovertarget="profile-pop"
    aria-expanded={menuOpen}
    aria-label={t("profile.menuLabel", { name: settings.profile })}
  >
    <ChevronDown size={15} strokeWidth={2.2} aria-hidden="true" class="transition-transform duration-200 ease-ui {menuOpen ? 'rotate-180' : ''}" />
  </button>
</div>

<Popover id="profile-pop" label={t("profile.popover")} width={332} height={590} bind:open={menuOpen}>
  <!-- Menu header: who you are and the device ID (a click copies it whole). -->
  <div class="flex items-center gap-3 px-4 pt-4 pb-3">
    <Avatar name={profiles.nickname} seed={id} size="md" />
    <div class="min-w-0 grow">
      <p class="truncate text-[15px] leading-tight font-semibold text-fg">
        {profiles.nickname || t("profile.noNickname")}
      </p>
      {#if id}
        <button
          type="button"
          class="mt-1 flex items-center gap-1.5 text-[12px] text-fg-2 hover:text-fg"
          onclick={copyId}
          aria-label={t("profile.copyId", { id: shortId(id) })}
          data-device-id
        >
          <span class="font-mono" aria-live="polite">{copied ? t("profile.idCopied") : shortId(id)}</span>
          {#if copied}<Check size={12} strokeWidth={2.2} class="text-live-text" aria-hidden="true" />{:else}<Copy size={12} strokeWidth={2} aria-hidden="true" />{/if}
        </button>
      {/if}
    </div>
  </div>

  <div class="pop-section px-2.5 pt-2 pb-2">
    <p class="pop-caption">{t("profile.listCaption")}</p>
    {#each profiles.list as p (p.name)}
      {@const current = p.name === settings.profile}
      <button
        type="button"
        class="menu-item"
        onclick={() => choose(p.name)}
        disabled={locked && !current}
        aria-current={current ? "true" : undefined}
      >
        <span class="grid size-4 shrink-0 place-items-center text-accent-text">
          {#if current}<Check size={15} strokeWidth={2.4} aria-hidden="true" />{/if}
        </span>
        <span class="min-w-0 grow">
          <span class="block truncate font-mono text-[13px]">{p.name}</span>
          <span class="mt-0.5 block truncate text-[12px] font-normal text-fg-2">{p.nickname}</span>
        </span>
        <span class="badge shrink-0 tabular-nums" aria-label={t("profile.friends", { n: p.allowed_devices })}>{p.allowed_devices}</span>
      </button>
    {/each}
    <button type="button" class="menu-item text-fg-2" onclick={openCreate} disabled={locked}>
      <span class="grid size-4 shrink-0 place-items-center"><Plus size={15} strokeWidth={2.2} aria-hidden="true" /></span>
      {t("profile.new")}
    </button>
  </div>

  <!-- Quick settings: tiles, the enabled one is blue. -->
  <div class="pop-section" data-plan>
    <p class="pop-caption">{t("profile.quick")}</p>
    <div class="mt-2 grid grid-cols-2 gap-2">
      <button
        type="button"
        role="switch"
        class="quick-tile"
        aria-checked={settings.theme === "dark"}
        onclick={() => settings.toggleTheme()}
        data-theme-tile
      >
        <Moon size={17} strokeWidth={2} aria-hidden="true" />
        <span class="min-w-0">
          <span class="block text-[13px] font-semibold">{t("profile.darkTheme")}</span>
          <span class="quick-tile-sub">{settings.theme === "dark" ? t("common.on") : t("common.off")}</span>
        </span>
      </button>
      <button type="button" class="quick-tile" onclick={() => showTab("look")} data-look-tile>
        <Palette size={17} strokeWidth={2} aria-hidden="true" />
        <span class="min-w-0">
          <span class="block text-[13px] font-semibold">{t("profile.look")}</span>
          <span class="quick-tile-sub">{t("profile.lookSub")}</span>
        </span>
      </button>
    </div>
    <!-- Background and language live on the "Settings" tab: the tiles lead there. -->
    <div class="mt-2 grid grid-cols-2 gap-2">
      <button type="button" class="quick-tile" onclick={() => showTab("settings")} data-bg-tile>
        <Image size={17} strokeWidth={2} aria-hidden="true" />
        <span class="min-w-0">
          <span class="block text-[13px] font-semibold">{t("bg.title")}</span>
          <span class="quick-tile-sub">{t("profile.bgSub")}</span>
        </span>
      </button>
      <button type="button" class="quick-tile" onclick={() => showTab("settings")} data-lang-tile>
        <Globe size={17} strokeWidth={2} aria-hidden="true" />
        <span class="min-w-0">
          <span class="block text-[13px] font-semibold">{t("profile.language")}</span>
          <span class="quick-tile-sub">{LANGUAGES.find((l) => l.code === language.current)?.native}</span>
        </span>
      </button>
    </div>
  </div>

  {#if locked}
    <p class="pop-section text-[12px] text-fg-3">{t("profile.lockedNote")}</p>
  {/if}
</Popover>

<Modal
  open={creating}
  label={t("profile.newLabel")}
  title={t("profile.new")}
  onclose={() => (creating = false)}
>
  <form id="new-profile" onsubmit={create}>
    <label class="label" for="new-name">{t("profile.nameLabel")}</label>
    <!-- svelte-ignore a11y_autofocus -->
    <input
      id="new-name"
      class="field h-11 font-mono"
      placeholder="friend-pc"
      spellcheck="false"
      autocomplete="off"
      maxlength="32"
      autofocus
      bind:value={newName}
      aria-invalid={newError !== null}
      aria-describedby={newError ? "new-error" : "new-hint"}
    />
    {#if newError}
      <p id="new-error" class="error-text" role="alert">{newError}</p>
    {/if}
    <p id="new-hint" class="mt-3 text-[13px] text-fg-2">
      {t("profile.newHint")}
    </p>
  </form>
  {#snippet actions()}
    <button type="button" class="btn btn-lg" onclick={() => (creating = false)}>
      {t("common.cancel")} <span class="kbd">Esc</span>
    </button>
    <button type="submit" form="new-profile" class="btn btn-primary btn-lg">
      {t("common.create")} <span class="kbd">Enter</span>
    </button>
  {/snippet}
</Modal>
