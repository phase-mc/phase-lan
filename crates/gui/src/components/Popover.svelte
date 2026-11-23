<script lang="ts">
  // A popover on the theme's dense surface (no backdrop-filter) built on the popover API: lives in the window's
  // top layer, closes by itself on an outside click and Esc (focus returns to the button). It is opened by
  // a button with popovertarget={id} — the browser tracks the state itself and doesn't confuse a click on the button
  // with an outside click. Position — below the button (or above it if there's no room below), at its right
  // or left edge.
  import type { Snippet } from "svelte";

  interface Props {
    id: string;
    /** Label for screen readers. */
    label: string;
    /** Which button edge to align to: right (end) or left (start). */
    align?: "start" | "end";
    width?: number;
    /** Approximate height: if it doesn't fit below, the popover opens upward. */
    height?: number;
    /** Called on every open (for example, to re-read a list). */
    onopen?: () => void;
    /** Whether the popover is open — for aria-expanded and button highlight (read-only). */
    open?: boolean;
    children: Snippet;
  }

  let {
    id,
    label,
    align = "end",
    width = 360,
    height = 360,
    onopen,
    open = $bindable(false),
    children,
  }: Props = $props();

  let el = $state<HTMLDivElement>();
  let side = $state<"bottom" | "top" | "right">("bottom");

  function place() {
    const trigger = document.querySelector<HTMLElement>(`[popovertarget="${id}"]`);
    if (!el || !trigger) return;
    const r = trigger.getBoundingClientRect();
    const gap = 10;
    const left =
      align === "end"
        ? Math.max(12, r.right - width)
        : Math.max(12, Math.min(r.left, window.innerWidth - width - 12));
    el.style.left = `${Math.round(left)}px`;
    const below = window.innerHeight - r.bottom - gap - 12;
    const above = r.top - gap - 12;
    // A short window (1020×640): it fits neither above nor below — to the right of the button,
    // full height, rather than scrolling inside.
    if (below < height && above < height && window.innerWidth - r.right - gap - 12 >= width) {
      side = "right";
      const top = Math.max(12, Math.min(r.top + r.height / 2 - height / 2, window.innerHeight - 12 - height));
      el.style.left = `${Math.round(r.right + gap)}px`;
      el.style.top = `${Math.round(top)}px`;
      el.style.bottom = "auto";
      el.style.maxHeight = `${Math.round(window.innerHeight - top - 12)}px`;
      return;
    }
    if (below >= height || below >= above) {
      side = "bottom";
      el.style.top = `${Math.round(r.bottom + gap)}px`;
      el.style.bottom = "auto";
      el.style.maxHeight = `${Math.round(below)}px`;
    } else {
      side = "top";
      el.style.top = "auto";
      el.style.bottom = `${Math.round(window.innerHeight - r.top + gap)}px`;
      el.style.maxHeight = `${Math.round(above)}px`;
    }
  }

  $effect(() => {
    const node = el;
    if (!node) return;
    const before = (e: Event) => {
      if ((e as ToggleEvent).newState === "open") place();
    };
    const toggle = (e: Event) => {
      open = (e as ToggleEvent).newState === "open";
      if (open) onopen?.();
    };
    // The window was resized — close the open popover: the button moved away.
    const resize = () => {
      if (node.matches(":popover-open")) node.hidePopover();
    };
    node.addEventListener("beforetoggle", before);
    node.addEventListener("toggle", toggle);
    window.addEventListener("resize", resize);
    return () => {
      node.removeEventListener("beforetoggle", before);
      node.removeEventListener("toggle", toggle);
      window.removeEventListener("resize", resize);
    };
  });
</script>

<div
  bind:this={el}
  {id}
  popover="auto"
  class="popover"
  data-side={side}
  role="dialog"
  aria-label={label}
  style:width="{width}px"
>
  {@render children()}
</div>
