<script lang="ts">
  // A question to the host: let the player in? There are 60 seconds to answer (the core decides this).
  // On top — a player card with a large nickname (who is knocking), below it — the device ID
  // in tiles for checking: the player names themselves, but access is granted by ID.
  // Keyboard: focus starts on "Decline" (the safe answer), Tab moves to "Let in",
  // Enter answers with the focused button, Esc — "Decline".
  import ShieldCheck from "@lucide/svelte/icons/shield-check";
  import { app } from "../lib/app.svelte";
  import { t } from "../lib/i18n/index.svelte";
  import Avatar from "./Avatar.svelte";
  import DeviceId from "./DeviceId.svelte";
  import Modal from "./Modal.svelte";

  const ANSWER_SECONDS = 60;

  const current = $derived(app.approvals[0]);
  const queued = $derived(app.approvals.length);
  let now = $state(Date.now());

  $effect(() => {
    if (!current) return;
    now = Date.now();
    const timer = setInterval(() => (now = Date.now()), 250);
    return () => clearInterval(timer);
  });

  const elapsed = $derived(current ? (now - current.receivedAt) / 1000 : 0);
  const left = $derived(Math.max(0, Math.ceil(ANSWER_SECONDS - elapsed)));
  const clock = $derived(`${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`);

  function answer(allow: boolean) {
    if (current) void app.answer(current.request_id, allow);
  }
</script>

<Modal
  open={current !== undefined}
  label={queued > 1 ? t("approval.labelQueued", { n: queued }) : t("approval.label")}
  title={t("approval.title")}
  onclose={() => answer(false)}
  width={520}
>
  {#snippet aside()}
    <p class="badge badge-mono {left <= 10 ? 'badge-red' : 'badge-blue'}" aria-live="off">
      <span class="sr-only">{t("approval.secondsLeft")} </span>{clock}
    </p>
  {/snippet}
  {#snippet band()}
    {#if current}
      {#key current.request_id}
        <!-- Timer: the bar drains evenly, like a time counter. -->
        <div class="h-1 bg-input" aria-hidden="true">
          <div
            class="countdown h-1 origin-left rounded-r-full"
            style:background={left <= 10 ? "var(--danger)" : "var(--cta)"}
            style:animation-duration="{ANSWER_SECONDS}s"
            style:animation-delay="-{Math.min(elapsed, ANSWER_SECONDS)}s"
          ></div>
        </div>
      {/key}
    {/if}
  {/snippet}

  {#if current}
    <!-- The main thing in the window is who is knocking: a player card with a large nickname. -->
    <div class="player-card flex items-center gap-4 p-4">
      <Avatar name={current.nickname} seed={current.device_id} size="lg" />
      <div class="min-w-0 grow">
        {#if current.nickname}
          <p class="truncate text-[24px] leading-tight font-semibold text-fg" data-nickname>
            {current.nickname}
          </p>
          <p class="mt-0.5 text-[14px] text-fg-2">{t("approval.wantsIn")}</p>
        {:else}
          <p class="text-[20px] leading-tight font-semibold text-fg">{t("approval.noNickname")}</p>
          <p class="mt-0.5 text-[14px] text-fg-2">
            {t("approval.wantsInOld")}
          </p>
        {/if}
      </div>
      <span class="badge badge-blue shrink-0 self-start">{t("approval.newPlayer")}</span>
    </div>

    <!-- The guarantee: the device ID. The player types the nickname, the ID can't be forged. -->
    <div class="mt-3 rounded-[12px] border border-line bg-card p-3.5">
      <div class="mb-2.5 flex items-center justify-between gap-3">
        <p class="text-[12px] font-medium text-fg-2">{t("approval.idCompare")}</p>
        <p class="flex items-center gap-1.5 text-[12px] font-medium text-live-text">
          <ShieldCheck size={14} strokeWidth={2} aria-hidden="true" />
          {t("approval.verify")}
        </p>
      </div>
      <DeviceId id={current.device_id} />
      <p class="mt-2.5 text-[12px] text-fg-3">
        {t("approval.idNote")}
      </p>
    </div>
    <p class="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-[12px] text-fg-3">
      <span><span class="kbd">Tab</span> {t("approval.keyChoose")}</span>
      <span><span class="kbd">Enter</span> {t("approval.keyAnswer")}</span>
      <span><span class="kbd">Esc</span> {t("approval.keyDecline")}</span>
    </p>
  {/if}

  {#snippet actions()}
    <!-- "Decline" first: the default focus is on the safe answer. -->
    <button type="button" class="btn btn-lg" onclick={() => answer(false)}>
      {t("approval.decline")} <span class="kbd">Esc</span>
    </button>
    <button type="button" class="btn btn-primary btn-lg" onclick={() => answer(true)}>
      {t("approval.allow")}
    </button>
  {/snippet}
</Modal>
