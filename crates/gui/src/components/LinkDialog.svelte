<script lang="ts">
  // A phase:// link arrived during a room or connection: we don't drop the session silently.
  // The question "Disconnect and go?": the main action is "Go" (the session stops,
  // the code from the link lands on the "Join" tab); Esc and "Stay" change nothing.
  // Even after "Go", connecting doesn't start by itself — the user starts it.
  import { app } from "../lib/app.svelte";
  import { t } from "../lib/i18n/index.svelte";
  import Modal from "./Modal.svelte";

  const link = $derived(app.pendingLink);
  const hosting = $derived(app.role === "host" || app.starting === "host");
</script>

<Modal
  open={link !== null}
  label={t("link.label")}
  title={t("link.title")}
  onclose={() => app.dismissPendingLink()}
>
  {#if link}
    <p>
      {#if link.kind === "join"}
        {t("link.room")}
        <span class="font-mono text-[13px] text-fg">{link.code}</span>.
      {:else}
        {t("link.invite")}
      {/if}
      {hosting
        ? t("link.closesRoom")
        : t("link.endsConnection")}
      {link.kind === "join" ? t("link.fillCode") : t("link.fillInvite")}
    </p>
  {/if}

  {#snippet actions()}
    <button type="button" class="btn btn-lg" onclick={() => app.dismissPendingLink()}>
      {t("link.stay")} <span class="kbd">Esc</span>
    </button>
    <button
      type="button"
      class="btn btn-primary btn-lg"
      disabled={app.stopping}
      onclick={() => void app.followPendingLink()}
    >
      {t("link.go")}
    </button>
  {/snippet}
</Modal>
