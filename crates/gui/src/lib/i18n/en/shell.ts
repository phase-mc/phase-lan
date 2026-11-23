// English: window frame and shared components.

import type { shell as ruShell } from "../ru/shell";
import type { Translated } from "../types";

export const shell: Translated<typeof ruShell> = {
  "topbar.sections": "Sections",
  "topbar.window": "Window",
  "topbar.minimize": "Minimize",
  "topbar.restore": "Restore",
  "topbar.maximize": "Maximize",
  "topbar.close": "Close",

  "count.steps": { one: "{n} step", other: "{n} steps" },
  "help.howToJoin": "How to join a world?",
  "help.voiceChat": "Voice chat",
  "error.title": "Error",
  "error.profile": "Profile “{name}”: {error}",
  "error.dismiss": "Dismiss error",
  "copy.label": "Copy",
  "copy.done": "Copied",
  "copy.failed": "Failed",
  "client.title": "Phase Client — coming soon",
  "client.text": "Its own game launcher, cosmetics and friends. Phase LAN will be inside.",
  "client.dismiss": "Dismiss announcement",
  "device.idLabel": "Device ID: {id}",
  "preview.spoken": "In game: {line}",
  "preview.spokenGlow": "In game: {line}, outline highlighted",

  "link.label": "Invite",
  "link.title": "Disconnect and open it?",
  "link.room": "You received a link to room",
  "link.invite": "You received a link with an invite to another room.",
  "link.closesRoom": "Your room will close and the guests will be disconnected.",
  "link.endsConnection": "Your current connection will end.",
  "link.fillCode": "The code will be filled in on the Join tab, and you connect yourself.",
  "link.fillInvite": "The invite will be filled in on the Join tab, and you connect yourself.",
  "link.stay": "Stay",
  "link.go": "Open it",

  "account.toggle": "Let players in without an account check",
  "account.hint":
    "In this mode, skins of licensed players do not load for others. A name is tied to the device that joined with it first. Needs Phase Companion in the host's game.",

  "companion.text":
    "Tab list header, colored names, more than 8 slots. Free, installed in the host's game, guests need nothing.",
  "companion.install": "Install",
  "companion.installing": "Installing…",
  "companion.doneRestart": "Done. Restart Minecraft once to activate the mod.",
  "companion.doneNext": "Done. The mod will work the next time the game starts.",
  "companion.upToDate": "Phase Companion is already installed, no updates.",
  "companion.noBuild": "There is no mod for this game version yet. We add versions one by one.",
  "companion.unknownLoader":
    "Could not find Fabric, Forge or NeoForge in this setup. Launch the game with one of them.",
  "companion.unknownGame": "Open a world in Minecraft: Phase will see the game version and pick the mod.",
  "companion.noModsFolder":
    "The mods folder was not found. Choose it in the Mod Sync card.",
};
