// Event journal: core events → short lines for the user.

import {
  shortId,
  type AccessDecision,
  type GuestReady,
  type HostReady,
  type PathKind,
  type Role,
  type SessionEvent,
  type TunnelEvent,
} from "./core";
import { t, type Key } from "./i18n/index.svelte";

export type Tone = "info" | "ok" | "warn" | "error";

export interface JournalDraft {
  tone: Tone;
  text: string;
}

const PATH: Record<PathKind, Key> = {
  direct: "path.direct",
  relay: "path.relay",
  other: "path.other",
};

export function pathLabel(path: PathKind | null): string {
  return path ? t(PATH[path]) : "—";
}

/** How to name a device in the journal: "Nick (#1603817a)" or "#1603817a" without a nick. */
export type Who = (peer: string) => string;

const ACCESS: Record<AccessDecision, { tone: Tone; key: Key }> = {
  allowed: { tone: "ok", key: "journal.access.allowed" },
  denied: { tone: "info", key: "journal.access.denied" },
  already_denied: { tone: "info", key: "journal.access.alreadyDenied" },
  prompts_off: { tone: "info", key: "journal.access.promptsOff" },
  busy: { tone: "info", key: "journal.access.busy" },
  timeout: { tone: "warn", key: "journal.access.timeout" },
  guest_left: { tone: "info", key: "journal.access.guestLeft" },
  room_full: { tone: "warn", key: "journal.access.roomFull" },
};

/** "Player GrayIsko1337 (#df7b5260) connected directly." The path is not yet known — without it. */
export function describeJoined(who: string, path: PathKind | null): JournalDraft {
  return {
    tone: "ok",
    text: path
      ? t("journal.joinedVia", { who, path: t(PATH[path]) })
      : t("journal.joined", { who }),
  };
}

function describeTunnel(e: TunnelEvent, role: Role | null, who: Who): JournalDraft | null {
  switch (e.kind) {
    // The "connected" line is written by the window state: together with the route (describeJoined).
    case "peer_connected":
      return null;
    case "peer_nickname":
      return {
        tone: "info",
        text: t("journal.introduced", { id: shortId(e.peer), nickname: e.nickname }),
      };
    case "peer_game_name":
      return {
        tone: "info",
        text: t("journal.gameName", { who: who(e.peer), name: e.name }),
      };
    case "peer_disconnected":
      return { tone: "info", text: t("journal.disconnected", { who: who(e.peer) }) };
    case "path_selected":
      return {
        tone: "info",
        text:
          role === "guest"
            ? t("journal.pathGuest", { path: t(PATH[e.path]) })
            : t("journal.pathHost", { who: who(e.peer), path: t(PATH[e.path]) }),
      };
    case "access": {
      const { tone, key } = ACCESS[e.decision];
      return { tone, text: t(key, { who: who(e.peer) }) };
    }
    case "game_unreachable":
      return { tone: "warn", text: t("journal.gameUnreachable", { port: e.port }) };
    case "host_lost":
      return { tone: "warn", text: t("journal.hostLost") };
    case "host_restored":
      return { tone: "ok", text: t("journal.hostRestored") };
    case "voice_summary":
      return {
        tone: "info",
        text: t("journal.voice", {
          sent: e.counts.sent,
          received: e.counts.received,
          dropped: e.counts.dropped,
        }),
      };
    // Ping is shown on screen, failed reconnection attempts — in the status;
    // mods handed to guests are counted on the "Mods" tab (a line per file is noise).
    case "ping":
    case "reconnect_failed":
    case "mod_sent":
      return null;
  }
}

/** A journal line for an event (`null` — the event doesn't go to the journal). */
export function describeEvent(
  event: SessionEvent,
  role: Role | null,
  who: Who,
): JournalDraft | null {
  switch (event.type) {
    case "notice":
      return { tone: event.data.level, text: event.data.text };
    case "world_search":
      return { tone: "info", text: t("journal.worldSearch", { s: event.data.timeout_s }) };
    case "world_found":
      return {
        tone: "ok",
        text: t("journal.worldFound", {
          name: event.data.name,
          port: event.data.port,
          summary: event.data.summary,
        }),
      };
    case "world_moved":
      return event.data.followed
        ? {
            tone: "ok",
            text: t("journal.worldMovedFollowed", { name: event.data.name, to: event.data.to }),
          }
        : {
            tone: "warn",
            text: t("journal.worldMovedManual", {
              name: event.data.name,
              to: event.data.to,
              from: event.data.from,
            }),
          };
    case "game_checked":
      // If the server didn't answer, the core sends a separate warning.
      return event.data.summary
        ? {
            tone: "ok",
            text: t("journal.gameChecked", { port: event.data.port, summary: event.data.summary }),
          }
        : null;
    case "approval_requested":
      return {
        tone: "info",
        text: t("journal.approvalRequested", { who: who(event.data.device_id) }),
      };
    case "approval_closed":
      return null;
    case "report_saved":
      return { tone: "info", text: t("journal.reportSaved", { path: event.data.path }) };
    case "tunnel":
      return describeTunnel(event.data, role, who);
  }
}

const ports = (list: number[]) => list.join(", ");

/** What is open on the host — journal lines at the moment the room is ready. */
export function describeHostReady(host: HostReady): JournalDraft[] {
  const lines: JournalDraft[] = [{ tone: "ok", text: t("journal.host.open") }];
  lines.push(
    host.game_port
      ? {
          tone: "info",
          text: t("journal.host.world", { motd: host.motd, port: host.game_port }),
        }
      : { tone: "warn", text: t("journal.host.noPort") },
  );
  if (host.voice.mode === "off") {
    lines.push({ tone: "info", text: t("journal.host.voiceOff") });
  } else if (host.voice.ports.length === 0) {
    lines.push({ tone: "warn", text: t("journal.host.voiceNone") });
  } else {
    lines.push({
      tone: "info",
      text: t("journal.host.voicePorts", { ports: ports(host.voice.ports.map((p) => p.port)) }),
    });
  }
  for (const hint of host.voice.hints) lines.push({ tone: "info", text: hint });
  if (host.relay.loopback_only) {
    lines.push({ tone: "info", text: t("journal.host.loopback") });
  } else if (!host.relay.custom) {
    lines.push({ tone: "info", text: t("journal.host.relayFree") });
  } else {
    lines.push({ tone: "info", text: t("journal.host.relayCustom", { urls: host.relay.urls.join(", ") }) });
  }
  return lines;
}

/** What is open on the guest — journal lines at the moment of connecting. */
export function describeGuestReady(guest: GuestReady): JournalDraft[] {
  const lines: JournalDraft[] = [
    {
      tone: "ok",
      text: guest.host_nickname
        ? t("journal.guest.connectedHost", {
            motd: guest.motd,
            host: guest.host_nickname,
            id: shortId(guest.host_id),
          })
        : t("journal.guest.connected", { motd: guest.motd }),
    },
  ];
  lines.push(
    guest.lan_name
      ? {
          tone: "info",
          text: t("journal.guest.lan", { lan: guest.lan_name, addr: guest.local_addr }),
        }
      : { tone: "info", text: t("journal.guest.addr", { addr: guest.local_addr }) },
  );
  if (guest.voice_ports.length > 0) {
    lines.push({
      tone: "info",
      text: t("journal.guest.voice", { ports: ports(guest.voice_ports) }),
    });
  }
  return lines;
}
