<!--
  Joining the world without an account check (like online-mode=false for a LAN world).
  Free for everyone, on by default (owner's decision). Works through Phase
  Companion; Phase pins each nickname to the device that joined with it first.
  A separate component: independent of styling and lives outside its block.
-->
<script lang="ts">
  import { t } from "../lib/i18n/index.svelte";
  import { look } from "../lib/look.svelte";

  const open = $derived(!look.settings.account_check);
</script>

<button
  type="button"
  role="switch"
  class="toggle-chip -mx-2.5 w-[calc(100%+20px)] justify-between"
  aria-checked={open}
  aria-describedby="account-check-hint"
  onclick={() => look.update({ account_check: open })}
  data-account-check
>
  {t("account.toggle")}
  <span class="mini-switch" aria-hidden="true"></span>
</button>
<p id="account-check-hint" class="mt-1 text-[12px] text-fg-3">
  {t("account.hint")}
</p>
