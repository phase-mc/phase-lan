// Window state: the current session, log, system log, guests and ping, "let them in?" questions.
// One object for the whole app; tabs only read it and call actions.

import {
  core,
  isFinished,
  onAutoRoom,
  onEvent,
  onLink,
  onLog,
  onRoomCode,
  onStatus,
  peerLabel,
  type PhaseLink,
  type HostRequest,
  type JoinRequest,
  type LogLine,
  type ModAction,
  type ModSyncStatus,
  type PathKind,
  type PendingApproval,
  type Role,
  type RoomCode,
  type SessionEvent,
  type SessionStatus,
  type TunnelEvent,
} from "./core";
import {
  describeEvent,
  describeGuestReady,
  describeHostReady,
  describeJoined,
  type JournalDraft,
  type Tone,
} from "./journal";
import { t } from "./i18n/index.svelte";
import { codeFromInput } from "./invite";
import { nav } from "./nav.svelte";

export interface JournalEntry {
  id: number;
  at: Date;
  tone: Tone;
  text: string;
}

/** Link to the device on the other side (for the host — guests, for a guest — the host):
 *  route and ping. They arrive as events — they change every couple of seconds. */
export interface Link {
  path: PathKind | null;
  rttMs: number | null;
}

/** A guest in the host's room: who (from the core state) and how the link is doing (from events). */
export interface GuestView extends Link {
  id: string;
  /** The guest names themselves; `null` — they have an old version without nicknames. */
  nickname: string | null;
  connections: number;
}

/** How long to wait for a new guest's route, to log "connected directly" as a single line. */
const JOIN_LINE_WAIT_MS = 1500;

export interface Approval extends PendingApproval {
  /** When the question appeared (ms): the host has 60 seconds to answer. */
  receivedAt: number;
}

/** The host's Minecraft world: searching, found, port check. */
export type WorldState =
  | { kind: "searching"; timeoutS: number }
  | { kind: "found"; name: string; port: number; summary: string }
  | { kind: "checked"; port: number; summary: string | null; error: string | null }
  /** The world is open on a different port, but the room points to a manually set port. */
  | { kind: "elsewhere"; name: string; port: number; current: number };

const JOURNAL_KEEP = 300;
const LOGS_KEEP = 500;

class AppStore {
  status = $state<SessionStatus | null>(null);
  journal = $state<JournalEntry[]>([]);
  /** The core's system log (tracing): newest on top. Not cleared between sessions. */
  logs = $state<LogLine[]>([]);
  /** What is known about the Minecraft world in the current host session. */
  world = $state<WorldState | null>(null);
  links = $state<Record<string, Link>>({});
  approvals = $state<Approval[]>([]);
  /** How many devices the host allowed this session (beyond the list at startup). */
  allowedAdded = $state(0);
  /** Error of the last action or session — the bar under the top row. */
  error = $state<string | null>(null);
  /** A session is starting (command sent, screen not yet changed). */
  starting = $state<Role | null>(null);
  stopping = $state(false);
  /** Ticket on the "Join" tab: lives between tabs, never written to disk. */
  draftTicket = $state("");
  /** A phase:// link that arrived during a session: waits for the "Disconnect and go?" answer. */
  pendingLink = $state<PhaseLink | null>(null);
  /** The room code the last invite link filled in: "Join" says where the code came from. */
  linkCode = $state<string | null>(null);

  #roomCode = $state<RoomCode | null>(null);
  #nextEntry = 1;
  #role: Role | null = null;
  /** Which session already has "what is open" written to the log. */
  #announced = 0;
  /** Nicknames of this session's devices — so the log names players by name. */
  #names = new Map<string, string>();
  /** Guests whose "connected" is not yet logged: waiting for the route (see JOIN_LINE_WAIT_MS). */
  #joining = new Map<string, ReturnType<typeof setTimeout>>();

  get active(): boolean {
    return this.status !== null && !isFinished(this.status.phase);
  }

  /** Room guests (host only): the core maintains the list, route and ping come from events. */
  get guests(): GuestView[] {
    const host = this.role === "host" ? this.status?.host : null;
    return (host?.guests ?? []).map((g) => ({
      id: g.device_id,
      nickname: g.nickname,
      connections: g.connections,
      path: this.links[g.device_id]?.path ?? null,
      rttMs: this.links[g.device_id]?.rttMs ?? null,
    }));
  }

  /** Players waiting in line behind the "Let in?" question on screen (host only). */
  get queued(): number {
    return this.role === "host" ? (this.status?.host?.queued ?? 0) : 0;
  }

  /** Short room code (host only, this session only). */
  get roomCode(): RoomCode | null {
    const code = this.#roomCode;
    return this.role === "host" && code && code.session === this.status?.id ? code : null;
  }

  /** Link to the host (guest only). */
  get hostLink(): Link | null {
    return this.role === "guest" ? (Object.values(this.links)[0] ?? null) : null;
  }

  /** Mod sync on the guest side (`null` — we are not a guest or haven't connected yet). */
  get modSync(): ModSyncStatus | null {
    return this.role === "guest" ? (this.status?.guest?.mods ?? null) : null;
  }

  /** The guest must decide what to do about mods: the screen's main action is sync. */
  get modsWaiting(): boolean {
    const state = this.modSync?.state;
    return state === "needs_sync" || state === "failed";
  }

  /** Role of the running session (`null` — no session). */
  get role(): Role | null {
    return this.active && this.status ? this.status.role : null;
  }

  /** Subscription to core events. Returns an unsubscribe function. */
  init(): () => void {
    const unlisten = [
      onStatus((s) => this.#applyStatus(s)),
      onEvent((e) => this.#onEvent(e)),
      onLog((line) => {
        this.logs.unshift(line);
        if (this.logs.length > LOGS_KEEP) this.logs.length = LOGS_KEEP;
      }),
      onLink(() => void this.#takeLink()),
      onAutoRoom((s) => this.#autoRoom(s)),
      onRoomCode((c) => (this.#roomCode = c)),
    ];
    void core.roomCode().then(
      (c) => (this.#roomCode = c),
      () => {},
    );
    // The link the window was launched with (or one that arrived while the page was loading).
    void this.#takeLink();
    void core.recentLogs().then((lines) => {
      // The subscription may already have brought lines — old ones from the buffer go after them.
      const seen = new Set(this.logs.map((l) => `${l.unix_ms}|${l.message}`));
      const older = lines.filter((l) => !seen.has(`${l.unix_ms}|${l.message}`)).reverse();
      this.logs = [...this.logs, ...older].slice(0, LOGS_KEEP);
    });
    // State at the moment the window opened.
    void core.status().then((s) => this.#applyStatus(s));
    void core.pendingApprovals().then((pending) => {
      const known = new Set(this.approvals.map((a) => a.request_id));
      const now = Date.now();
      this.approvals.push(
        ...pending.filter((p) => !known.has(p.request_id)).map((p) => ({ ...p, receivedAt: now })),
      );
    });
    return () => unlisten.forEach((p) => void p.then((off) => off()));
  }

  startHost(request: HostRequest): Promise<void> {
    return this.#start("host", () => core.startHost(request));
  }

  join(request: JoinRequest): Promise<void> {
    return this.#start("guest", () => core.join(request));
  }

  /** Stops the session: connections close, tabs return to their initial look. */
  async stop(): Promise<void> {
    if (this.stopping) return;
    this.stopping = true;
    try {
      this.#applyStatus(await core.stop());
    } catch (e) {
      this.error = String(e);
    } finally {
      this.stopping = false;
    }
  }

  async answer(requestId: number, allow: boolean): Promise<void> {
    this.approvals = this.approvals.filter((a) => a.request_id !== requestId);
    try {
      await core.answerApproval(requestId, allow);
    } catch (e) {
      this.error = String(e);
    }
  }

  /** Invitation link: the code or ticket is filled in on the "Join" tab.
   *  Connecting doesn't start by itself — anyone could have sent the link, and a connection
   *  reveals the IP. During a session we first ask "Disconnect and go?". */
  openLink(link: PhaseLink): void {
    if (link.kind === "broken") {
      // Nothing to fill in. A running session stays where it is; the bar explains the link.
      if (!this.active && !this.starting) nav.go("join");
      this.error = t("link.broken");
      return;
    }
    // The room this window is already in: the host opened their own link to check it, or the
    // guest clicked the same link again. Nothing to ask, just show that room.
    const role = this.role ?? this.starting;
    if (role && this.#isCurrentRoom(role, link)) {
      nav.go(role === "host" ? "host" : "join");
      return;
    }
    if (this.active || this.starting) {
      this.pendingLink = link;
      return;
    }
    this.#fillLink(link);
  }

  #isCurrentRoom(role: Role, link: PhaseLink): boolean {
    if (role === "host") return link.kind === "join" && link.code === this.roomCode?.code;
    if (link.kind === "join") return link.code === codeFromInput(this.draftTicket);
    return link.kind === "ticket" && link.ticket === this.draftTicket.trim();
  }

  /** "Go": stop the session and fill in the code from the link. */
  async followPendingLink(): Promise<void> {
    const link = this.pendingLink;
    this.pendingLink = null;
    if (!link) return;
    await this.stop();
    if (!this.active) this.#fillLink(link);
  }

  dismissPendingLink(): void {
    this.pendingLink = null;
  }

  #fillLink(link: PhaseLink): void {
    if (link.kind === "broken") return;
    this.draftTicket = link.kind === "join" ? link.code : link.ticket;
    this.linkCode = link.kind === "join" ? codeFromInput(link.code) : null;
    this.error = null;
    nav.go("join");
  }

  async #takeLink(): Promise<void> {
    try {
      const link = await core.takeLink();
      if (link) this.openLink(link);
    } catch {
      // A window without a core (vite in a browser) — no links.
    }
  }

  /** A guest's mod action. The result arrives in the session state (`guest.mods`). */
  async modAction(action: ModAction): Promise<void> {
    try {
      await core.modCommand(action);
    } catch (e) {
      this.error = String(e);
    }
  }

  async #start(role: Role, run: () => Promise<SessionStatus>): Promise<void> {
    if (this.starting || this.active) return;
    this.#reset(role);
    this.starting = role;
    try {
      this.#applyStatus(await run());
    } catch (e) {
      this.error = String(e);
    } finally {
      this.starting = null;
    }
  }

  /** The auto-room created the room: a new host session without a button press. */
  #autoRoom(status: SessionStatus): void {
    // This session's state may have arrived earlier (core://status) — then the log is already its own.
    if (!this.status || status.id > this.status.id) this.#reset("host");
    this.#applyStatus(status);
    nav.go("host");
  }

  /** A new session — a clean log. Reset before starting: the first core events may
   *  arrive before the command's reply. */
  #reset(role: Role): void {
    this.journal = [];
    this.links = {};
    this.#names.clear();
    this.#joining.forEach((timer) => clearTimeout(timer));
    this.#joining.clear();
    this.approvals = [];
    this.allowedAdded = 0;
    this.world = null;
    this.error = null;
    this.#role = role;
  }

  #applyStatus(next: SessionStatus | null): void {
    if (!next) return;
    const current = this.status;
    // State arrives two ways (command reply and event) — the order is not guaranteed.
    // A stale copy is dropped by session number and change number.
    if (current) {
      if (next.id < current.id) return;
      if (next.id === current.id && next.rev < current.rev) return;
    }
    const failedNow =
      next.phase === "failed" && !(current?.id === next.id && current.phase === "failed");
    this.status = next;
    this.#role = next.role;
    if (failedNow) this.error = next.error ?? t("app.sessionFailed");
    for (const g of next.host?.guests ?? []) this.#name(g.device_id, g.nickname);
    if (next.guest) this.#name(next.guest.host_id, next.guest.host_nickname);

    if (this.#announced !== next.id && !isFinished(next.phase)) {
      const lines = next.host
        ? describeHostReady(next.host)
        : next.guest
          ? describeGuestReady(next.guest)
          : null;
      if (lines) {
        this.#announced = next.id;
        lines.forEach((line) => this.#push(line));
      }
    }
  }

  #onEvent(event: SessionEvent): void {
    let log = true;
    if (event.type === "approval_requested") {
      this.#name(event.data.device_id, event.data.nickname);
      this.approvals.push({ ...event.data, receivedAt: Date.now() });
    } else if (event.type === "approval_closed") {
      this.approvals = this.approvals.filter((a) => a.request_id !== event.data.request_id);
    } else if (event.type === "world_search") {
      this.world = { kind: "searching", timeoutS: event.data.timeout_s };
    } else if (event.type === "world_found") {
      this.world = { kind: "found", ...event.data };
    } else if (event.type === "game_checked") {
      this.world = { kind: "checked", ...event.data };
    } else if (event.type === "world_moved") {
      const { from, to, name, summary } = event.data;
      // The room moved to a new port — the world is found again, the "port is silent" warning goes away.
      this.world = event.data.followed
        ? { kind: "found", name, port: to, summary }
        : { kind: "elsewhere", name, port: to, current: from };
    } else if (event.type === "tunnel") {
      log = this.#onTunnel(event.data);
    }
    const draft = log ? describeEvent(event, this.#role, (peer) => this.#who(peer)) : null;
    if (draft) this.#push(draft);
  }

  /** Remembers the device's nickname (if it gave one). */
  #name(peer: string, nickname: string | null | undefined): void {
    if (nickname) this.#names.set(peer, nickname);
  }

  /** "Nick (#1603817a)" or "#1603817a" — how to name the device in the log. */
  #who(peer: string): string {
    return peerLabel(peer, this.#names.get(peer));
  }

  /** Writes "Player … connected …" to the log — once per connection. */
  #announceJoin(peer: string): void {
    const timer = this.#joining.get(peer);
    if (timer === undefined) return;
    clearTimeout(timer);
    this.#joining.delete(peer);
    this.#push(describeJoined(this.#who(peer), this.links[peer]?.path ?? null));
  }

  /** Updates links. Returns `false` if the event shouldn't be logged. */
  #onTunnel(e: TunnelEvent): boolean {
    switch (e.kind) {
      case "peer_connected": {
        this.#name(e.peer, e.nickname);
        // A new connection — route and ping will arrive again.
        this.links[e.peer] = { path: null, rttMs: null };
        // The route follows: then the line reads "connected directly".
        // If it doesn't come — we write it without.
        this.#announceJoin(e.peer);
        this.#joining.set(
          e.peer,
          setTimeout(() => this.#announceJoin(e.peer), JOIN_LINE_WAIT_MS),
        );
        return true;
      }
      case "peer_nickname": {
        this.#name(e.peer, e.nickname);
        // The "connected" line isn't written yet — the nickname goes straight into it.
        return !this.#joining.has(e.peer);
      }
      case "peer_disconnected":
        this.#announceJoin(e.peer);
        return true;
      case "path_selected": {
        const link = this.#link(e.peer);
        const changed = link.path !== e.path;
        link.path = e.path;
        if (this.#joining.has(e.peer)) {
          this.#announceJoin(e.peer);
          return false;
        }
        return changed;
      }
      case "ping": {
        const selected = e.paths.find((p) => p.selected);
        const link = this.#link(e.peer);
        link.rttMs = selected ? selected.rtt_ms : null;
        if (selected) link.path = selected.path;
        return true;
      }
      case "host_lost":
        // No link — no ping and no path. After recovery the core sends the path again.
        for (const link of Object.values(this.links)) {
          link.rttMs = null;
          link.path = null;
        }
        return true;
      case "access":
        this.#name(e.peer, e.nickname);
        if (e.decision === "allowed") this.allowedAdded += 1;
        return true;
      default:
        return true;
    }
  }

  #link(peer: string): Link {
    this.links[peer] ??= { path: null, rttMs: null };
    return this.links[peer];
  }

  #push(draft: JournalDraft): void {
    this.journal.unshift({ id: this.#nextEntry++, at: new Date(), ...draft });
    if (this.journal.length > JOURNAL_KEEP) this.journal.length = JOURNAL_KEEP;
  }
}

export const app = new AppStore();
