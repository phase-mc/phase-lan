<script lang="ts">
  // The "Network and relay" tab. Fields write straight into the shared settings store: the "Host" and
  // "Join" tabs see changes at once, and they save to disk by themselves (except the password).
  // A running session isn't restarted: new settings take effect from the next launch.
  import Globe from "@lucide/svelte/icons/globe";
  import Info from "@lucide/svelte/icons/info";
  import Power from "@lucide/svelte/icons/power";
  import Radio from "@lucide/svelte/icons/radio";
  import Server from "@lucide/svelte/icons/server";
  import { onMount } from "svelte";
  import CopyButton from "../components/CopyButton.svelte";
  import { app } from "../lib/app.svelte";
  import { core, shortId, type NetworkMode } from "../lib/core";
  import { t, type Key } from "../lib/i18n/index.svelte";
  import { profiles } from "../lib/profiles.svelte";
  import { settings } from "../lib/settings.svelte";

  const MODES: { id: NetworkMode; title: Key; text: Key }[] = [
    { id: "normal", title: "net.modeNormal", text: "net.modeNormalSub" },
    { id: "relay_only", title: "net.modeRelay", text: "net.modeRelaySub" },
    { id: "loopback_only", title: "network.local", text: "net.modeLocalSub" },
  ];

  // ---------------------------------------------------------------- device
  // The technical state of the device and relay is shown only here: the first screen doesn't carry it.
  const session = $derived(app.active ? app.status : null);
  const relay = $derived((session?.host ?? session?.guest)?.relay ?? null);
  const deviceId = $derived(session?.device_id ?? profiles.current?.device_id ?? null);
  const keyError = $derived(profiles.error ?? profiles.current?.error ?? null);

  const node = $derived.by((): { dot: string; text: string } => {
    if (keyError) return { dot: "dot-danger", text: t("net.keyUnread") };
    if (!session) return { dot: "dot-live", text: t("net.ready") };
    if (session.phase === "reconnecting") return { dot: "dot-warn dot-pulse", text: t("net.hostLost") };
    if (session.phase !== "running") return { dot: "dot-busy dot-pulse", text: t("net.connecting") };
    return {
      dot: "dot-live dot-pulse",
      text: session.role === "host" ? t("host.roomOpen") : t("net.connectedToHost"),
    };
  });

  const ROUTE: Record<"direct" | "relay" | "other", Key> = {
    direct: "net.routeDirect",
    relay: "net.routeRelay",
    other: "net.routeOther",
  };
  const route = $derived.by(() => {
    const live = app.hostLink;
    if (live?.path)
      return live.rttMs === null
        ? t(ROUTE[live.path])
        : `${t(ROUTE[live.path])} · ${t("room.ms", { n: Math.round(live.rttMs) })}`;
    if (settings.network === "loopback_only") return t("network.local");
    return settings.network === "relay_only" ? t("net.routeRelayOnly") : t("net.routeAuto");
  });

  // ---------------------------------------------------------------- Phase in the background
  // Autostart lives in the Windows registry (we read it from the core), the auto-room — in settings.
  let autostart = $state(false);
  let autostartBusy = $state(false);
  let autostartError = $state<string | null>(null);
  onMount(() => {
    core.autostartEnabled().then((on) => (autostart = on), () => {});
  });

  async function toggleAutostart() {
    if (autostartBusy) return;
    autostartBusy = true;
    autostartError = null;
    try {
      autostart = await core.setAutostart(!autostart);
    } catch (e) {
      autostartError = String(e);
    } finally {
      autostartBusy = false;
    }
  }

  const BACKGROUND = [
    {
      id: "auto-room",
      title: "net.autoRoom" as Key,
      text: "net.autoRoomSub" as Key,
      on: () => settings.autoRoom,
      toggle: () => (settings.autoRoom = !settings.autoRoom),
    },
    {
      id: "autostart",
      title: "net.autostart" as Key,
      text: "net.autostartSub" as Key,
      on: () => autostart,
      busy: () => autostartBusy,
      toggle: () => void toggleAutostart(),
    },
  ];

  const relayText = $derived.by(() => {
    if (relay?.loopback_only || (!relay && !settings.online)) return t("net.relayUnused");
    if (relay) return relay.custom ? relay.urls.join(", ") : t("net.relayShared");
    return settings.relayUrl.trim() || t("net.relayShared");
  });
</script>

<section class="page flex flex-col gap-5" aria-labelledby="net-title">
  <header class="flex items-end justify-between gap-6">
    <div>
      <h1 id="net-title" class="page-title">{t("nav.networkFull")}</h1>
      <p class="page-lead">{t("net.lead")}</p>
    </div>
    {#if app.active}
      <span class="pill"><span class="dot dot-busy" aria-hidden="true"></span>{t("net.sessionNote")}</span>
    {/if}
  </header>

  <section class="card p-5" aria-labelledby="node-title" data-node>
    <div class="flex items-center gap-3">
      <span class="icon-tile"><Radio size={20} strokeWidth={2} aria-hidden="true" /></span>
      <div class="min-w-0 grow">
        <h2 id="node-title" class="section-title">{t("net.thisDevice")}</h2>
        <p class="text-[13px] text-fg-2">{t("net.thisDeviceSub")}</p>
      </div>
      <span class="pill" aria-live="polite"><span class="dot {node.dot}" aria-hidden="true"></span>{node.text}</span>
    </div>
    {#if keyError}
      <p class="note note-danger mt-4" role="alert"><span>{keyError}</span></p>
    {/if}
    <dl class="mt-3 divide-y divide-[var(--border-subtle)]">
      <div class="row py-2">
        <dt>{t("net.route")}</dt>
        <dd>{route}</dd>
      </div>
      <div class="row py-2">
        <dt>Relay</dt>
        <dd class={relayText.startsWith("http") ? "font-mono" : ""}>{relayText}</dd>
      </div>
      <div class="row py-2">
        <dt>{t("net.deviceId")}</dt>
        <dd class="flex items-center justify-end gap-2">
          <span class="font-mono">{deviceId ? shortId(deviceId) : "—"}</span>
          {#if deviceId}
            <CopyButton text={deviceId} label={t("copy.label")} class="btn h-7 px-2.5 text-[12px]" icon={12} />
          {/if}
        </dd>
      </div>
    </dl>
  </section>

  <section class="card p-5" aria-labelledby="bg-title" data-background>
    <div class="flex items-center gap-3">
      <span class="icon-tile"><Power size={20} strokeWidth={2} aria-hidden="true" /></span>
      <div>
        <h2 id="bg-title" class="section-title">{t("net.background")}</h2>
        <p class="text-[13px] text-fg-2">{t("net.backgroundSub")}</p>
      </div>
    </div>
    <div class="mt-3 divide-y divide-[var(--border-subtle)]">
      {#each BACKGROUND as item (item.id)}
        <div class="row gap-4 py-2.5">
          <div class="min-w-0">
            <p class="text-[14px] font-medium text-fg">{t(item.title)}</p>
            <p class="text-[13px] text-fg-2">{t(item.text)}</p>
          </div>
          <button
            type="button"
            role="switch"
            class="toggle-chip toggle-chip-solid"
            aria-checked={item.on()}
            aria-label={t(item.title)}
            disabled={item.busy?.()}
            onclick={item.toggle}
          >
            <span class="mini-switch" aria-hidden="true"></span>
            <span class="w-7 text-left" aria-hidden="true">{item.on() ? t("common.on") : t("common.off")}</span>
          </button>
        </div>
      {/each}
    </div>
    {#if autostartError}
      <p class="note note-danger mt-3" role="alert"><span>{autostartError}</span></p>
    {/if}
  </section>

  <fieldset class="card p-5">
    <legend class="sr-only">{t("net.mode")}</legend>
    <div class="flex items-center gap-3">
      <span class="icon-tile"><Globe size={20} strokeWidth={2} aria-hidden="true" /></span>
      <div>
        <h2 class="section-title">{t("net.mode")}</h2>
        <p class="text-[13px] text-fg-2">{t("net.modeSub")}</p>
      </div>
    </div>
    <div class="mt-4 grid grid-cols-3 gap-3">
      {#each MODES as mode (mode.id)}
        <label class="choice">
          <input type="radio" class="sr-only" bind:group={settings.network} value={mode.id} />
          <span class="flex items-center gap-2.5 text-[14px] font-semibold text-fg">
            <span class="radio" aria-hidden="true"></span>
            {t(mode.title)}
          </span>
          <span class="mt-1.5 block pl-[26px] text-[13px] text-fg-2">{t(mode.text)}</span>
        </label>
      {/each}
    </div>
  </fieldset>

  <fieldset class="card p-5 transition-opacity duration-200 ease-ui disabled:opacity-55" disabled={!settings.online}>
    <legend class="sr-only">{t("net.ownRelay")}</legend>
    <div class="flex items-center gap-3">
      <span class="icon-tile"><Server size={20} strokeWidth={2} aria-hidden="true" /></span>
      <div>
        <h2 class="section-title">{t("net.ownRelay")}</h2>
        <p class="text-[13px] text-fg-2">
          {t("net.ownRelaySub")}
        </p>
      </div>
    </div>
    <div class="mt-4 grid grid-cols-2 gap-4">
      <div class="col-span-2">
        <label class="label" for="relay-url">{t("net.address")}</label>
        <input
          id="relay-url"
          class="field h-10 font-mono text-[13px]"
          placeholder="https://relay.example.com"
          spellcheck="false"
          autocomplete="off"
          bind:value={settings.relayUrl}
        />
      </div>
      <div>
        <label class="label" for="relay-token">{t("net.password")}</label>
        <input
          id="relay-token"
          type="password"
          class="field h-10"
          autocomplete="off"
          bind:value={settings.relayToken}
        />
      </div>
      <div>
        <label class="label" for="relay-ca">{t("net.certificate")}</label>
        <input
          id="relay-ca"
          class="field h-10 font-mono text-[13px]"
          placeholder="C:\Phase\relay-cert.pem"
          spellcheck="false"
          autocomplete="off"
          bind:value={settings.relayCa}
        />
      </div>
      <div class="col-span-2">
        <label class="label" for="codes-url">{t("net.codesService")}</label>
        <input
          id="codes-url"
          class="field h-10 font-mono text-[13px]"
          placeholder="http://relay.example.com:3341"
          spellcheck="false"
          autocomplete="off"
          bind:value={settings.codesUrl}
        />
        <p class="mt-1.5 text-[12px] text-fg-3">
          {t("net.codesHint")}
        </p>
      </div>
    </div>
  </fieldset>

  <section class="card p-5" aria-labelledby="about-title">
    <div class="flex items-center gap-3">
      <span class="icon-tile"><Info size={20} strokeWidth={2} aria-hidden="true" /></span>
      <h2 id="about-title" class="section-title">{t("net.program")}</h2>
    </div>
    <dl class="mt-3 divide-y divide-[var(--border-subtle)]">
      <div class="row py-2">
        <dt>{t("net.version")}</dt>
        <dd class="font-mono">
          {profiles.info ? t("net.versionValue", { version: profiles.info.app_version, protocol: profiles.info.protocol_version }) : "—"}
        </dd>
      </div>
      <div class="row py-2">
        <dt>{t("net.data")}</dt>
        <dd class="font-mono">{profiles.info?.data_dir ?? "—"}</dd>
      </div>
      <div class="row py-2">
        <dt>{t("net.profiles")}</dt>
        <dd class="font-mono tabular-nums">{profiles.list.length}</dd>
      </div>
    </dl>
  </section>

  <p class="text-[12px] text-fg-3">
    Not an official Minecraft product. Not approved by or associated with Mojang or Microsoft.
  </p>
</section>
