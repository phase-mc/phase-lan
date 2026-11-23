// English: common words, formats, journal and stores (src/lib).

import type { lib as ruLib } from "../ru/lib";
import type { Translated } from "../types";

export const lib: Translated<typeof ruLib> = {
  "nav.host": "Host",
  "nav.join": "Join",
  "nav.mods": "Mods",
  "nav.journal": "Log",
  "nav.network": "Network",
  "nav.networkFull": "Network & relay",
  "nav.look": "Look",
  "nav.lookFull": "Room look",
  "nav.settings": "Settings",

  "size.mb": "{n} MB",
  "size.kb": "{n} KB",
  "count.mods": { one: "{n} mod", other: "{n} mods" },
  "count.slots": { one: "{n} slot", other: "{n} slots" },
  "count.players": { one: "{n} player", other: "{n} players" },

  "prefix.tooLong": "At most {max} characters. Now {len}.",
  "prefix.noSpaces": "No spaces.",
  "prefix.badChar": "The character “{ch}” is not allowed: letters, digits and - _ . ! + * # &.",
  "nickname.empty": "The nickname cannot be empty.",
  "nickname.tooLong": "A nickname is at most {max} characters. Now {len}.",
  "profile.nameRule": "Latin letters, digits, “-” and “_” only. Up to 32 characters.",
  "settings.portError":
    "A port is a number from 1 to 65535. Minecraft prints it in chat after “Open to LAN”.",

  "network.local": "This PC only",
  "network.relayCustom": "custom relay",
  "network.relayN0": "n0 relay",
  "network.relayOnly": "{relay} only",
  "network.direct": "Direct · {relay}",

  "path.direct": "directly",
  "path.relay": "via relay",
  "path.other": "via a fallback route",

  "app.sessionFailed": "The session ended with an error.",
  "look.noConnection": "There is no connection right now.",
  "modsDir.runningGame": "From the running game",
  "modsDir.chosen": "Chosen manually",
  "modsDir.default": "Default",
  "modsRestore.done": "Your mods are back: {restored}. Host mods removed: {removed}.",
  "modsRestore.conflicts": {
    one: "{n} mod stayed in the backup: a file with that name already exists in the folder.",
    other: "{n} mods stayed in the backup: files with those names already exist in the folder.",
  },
  "modsRestore.failed": {
    one: "{n} file is in use. Close Minecraft and try again.",
    other: "{n} files are in use. Close Minecraft and try again.",
  },

  "journal.access.allowed": "Player {who} was given access. Your answer is remembered.",
  "journal.access.denied": "You declined player {who}.",
  "journal.access.alreadyDenied": "Player {who} is asking for access again. They were already declined.",
  "journal.access.promptsOff": "Player {who} was declined: new devices are not accepted.",
  "journal.access.busy": "Player {who} was declined: you are still answering another request.",
  "journal.access.timeout": "You did not answer in time. Player {who} was declined.",
  "journal.access.guestLeft": "Player {who} left without waiting for an answer.",
  "journal.access.roomFull": "Player {who} could not join: the room has no free slots.",
  "journal.joinedVia": "Player {who} joined {path}.",
  "journal.joined": "Player {who} joined.",
  "journal.introduced": "Player {id} introduced themselves as {nickname}.",
  "journal.gameName": "Player {who} is entering the world as {name}.",
  "journal.disconnected": "Player {who} disconnected.",
  "journal.pathGuest": "The connection to the host goes {path}.",
  "journal.pathHost": "Player {who}: the connection goes {path}.",
  "journal.gameUnreachable":
    "The game on port {port} is not accepting connections. Is the world open to LAN?",
  "journal.hostLost": "Connection to the host lost. Reconnecting.",
  "journal.hostRestored": "Connection to the host restored.",
  "journal.voice": "Voice: {sent} sent, {received} received, {dropped} dropped packets.",
  "journal.worldSearch": "Looking for an open LAN world on this PC (up to {s} s).",
  "journal.worldFound": "Found world “{name}” on port {port} ({summary}).",
  "journal.worldMovedFollowed":
    "World “{name}” is open again, now on port {to}. Same room: friends join with the same code.",
  "journal.worldMovedManual":
    "World “{name}” is open on port {to}, but the room points to port {from}, set manually. Choose “Auto-detect” or port {to} and create the room again.",
  "journal.gameChecked": "Port {port}: {summary}.",
  "journal.approvalRequested": "Player {who} asks to join the room.",
  "journal.reportSaved": "Session report saved: {path}",
  "journal.host.open": "The room is open. The code for friends is ready.",
  "journal.host.world": "World “{motd}”: friends' connections go to the game on port {port}.",
  "journal.host.noPort": "No game port set: only the connection check runs.",
  "journal.host.voiceOff": "Voice (UDP) is not forwarded.",
  "journal.host.voiceNone": "Voice (UDP): no suitable ports.",
  "journal.host.voicePorts": "Voice (UDP): ports {ports}.",
  "journal.host.loopback": "“This PC only” mode: the guest is on this same computer.",
  "journal.host.relayFree": "Relay: free n0 servers. They are for testing only.",
  "journal.host.relayCustom": "Relay: {urls}.",
  "journal.guest.connectedHost": "Connected to “{motd}”, hosted by {host} ({id}).",
  "journal.guest.connected": "Connected to “{motd}”.",
  "journal.guest.lan":
    "The world is listed under LAN World in Minecraft as “{lan}”. Address: {addr}.",
  "journal.guest.addr": "Address for Minecraft: {addr}.",
  "journal.guest.voice": "Voice (UDP): ports {ports} are open on 127.0.0.1.",
};
