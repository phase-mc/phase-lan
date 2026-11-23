<script lang="ts">
  // A modal window on the system <dialog>: focus is trapped inside (Tab cycles through the buttons),
  // the background is inert. Esc calls onclose — what it means is up to the calling screen.
  // Look: a rounded card on a blurred background; a strip (timer) along the top edge,
  // caption and title, text, two buttons with a gap at the bottom.
  import type { Snippet } from "svelte";

  interface Props {
    open: boolean;
    /** Caption above the title. */
    label: string;
    title: string;
    /** Esc and the system closing the window. Without it the window closes only via its buttons. */
    onclose?: () => void;
    /** Right part of the caption row (counter, timer). */
    aside?: Snippet;
    /** A strip along the top edge of the window (for example, the time left). */
    band?: Snippet;
    children: Snippet;
    actions: Snippet;
    /** Window width, px. */
    width?: number;
  }

  let { open, label, title, onclose, aside, band, children, actions, width = 480 }: Props =
    $props();

  const uid = $props.id();
  let dialog = $state<HTMLDialogElement>();

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    else if (!open && dialog.open) dialog.close();
  });

  function oncancel(event: Event) {
    event.preventDefault();
    onclose?.();
  }

  const FOCUSABLE =
    'button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [href], [tabindex]:not([tabindex="-1"])';

  /** Tab cycles inside the window. WebView2 has no browser chrome: without this, focus
   *  leaves the last button for the empty document and Enter stops responding. */
  function trapTab(event: KeyboardEvent) {
    if (!open || !dialog?.open || event.key !== "Tab") return;
    const items = [...dialog.querySelectorAll<HTMLElement>(FOCUSABLE)];
    if (items.length === 0) return;
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    const inside = active instanceof HTMLElement && dialog.contains(active);
    if (!inside) {
      event.preventDefault();
      (event.shiftKey ? last : first).focus();
    } else if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  function onclosed() {
    // The browser may close the window itself (a repeated Esc). If the question is still open — we decide
    // the same way as on Esc; without a handler we put the window back.
    if (!open || !dialog) return;
    if (onclose) onclose();
    else dialog.showModal();
  }
</script>

<svelte:window onkeydown={trapTab} />

<dialog
  bind:this={dialog}
  {oncancel}
  onclose={onclosed}
  aria-labelledby="{uid}-title"
  aria-describedby="{uid}-body"
  class="modal"
  style:width="min({width}px, calc(100vw - 32px))"
>
  {#if open}
    {@render band?.()}
    <div class="px-6 pt-6 pb-5">
      <header class="flex min-h-6 items-center justify-between gap-4">
        <p class="eyebrow">{label}</p>
        {@render aside?.()}
      </header>
      <h2 id="{uid}-title" class="mt-1 text-[20px] leading-tight font-semibold text-fg">{title}</h2>
      <div id="{uid}-body" class="mt-3 text-[14px] text-fg-2">{@render children()}</div>
    </div>
    <footer class="flex gap-3 px-6 pb-6 [&>*]:flex-1">
      {@render actions()}
    </footer>
  {/if}
</dialog>
