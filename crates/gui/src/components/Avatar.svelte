<script lang="ts">
  // Player avatar: a circle with the nickname's first letter. The circle's gradient comes from the device ID (the same
  // player is always the same color, even after changing nickname), all shades are blue-icy.
  // Without a nickname — a silhouette. The dot in the corner is the link state.
  import User from "@lucide/svelte/icons/user";

  interface Props {
    /** Player nickname (`null` — not named). */
    name: string | null | undefined;
    /** Device ID: the color depends on it. */
    seed?: string | null;
    size?: "xs" | "sm" | "md" | "lg";
    /** Class of the state dot in the corner (for example, `dot-live`); without it there is no dot. */
    dot?: string;
  }

  let { name, seed = null, size = "md", dot }: Props = $props();

  const TINTS = ["av-cobalt", "av-sky", "av-cyan", "av-navy"];
  const SIZE = {
    xs: "size-6 text-[11px]",
    sm: "size-7 text-[12px]",
    md: "size-9 text-[15px]",
    lg: "size-14 text-[22px]",
  };
  const ICON = { xs: 13, sm: 14, md: 18, lg: 26 };

  const tint = $derived.by(() => {
    let hash = 0;
    for (const ch of seed ?? name ?? "") hash = (hash * 31 + ch.codePointAt(0)!) >>> 0;
    return TINTS[hash % TINTS.length];
  });
  /** First letter or digit of the nickname. */
  const initial = $derived(name?.match(/[\p{L}\p{N}]/u)?.[0]?.toUpperCase() ?? null);
</script>

<span class="avatar {tint} {SIZE[size]}" aria-hidden="true">
  {#if initial}
    {initial}
  {:else}
    <User size={ICON[size]} strokeWidth={2} />
  {/if}
  {#if dot}
    <!-- Ring around the dot — the surface color under the avatar (--avatar-ring is set by the parent). -->
    <span class="dot {dot} absolute -right-0.5 -bottom-0.5 ring-2 ring-[var(--avatar-ring,var(--bg-app))]"></span>
  {/if}
</span>
