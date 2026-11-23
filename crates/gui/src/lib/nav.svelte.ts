// Window tabs (open text tabs in the center of the top row). A tab doesn't depend
// on the session: during a room or connection you can go to "Journal" or "Network" and
// come back — the session runs in the background.

import type { Key } from "./i18n/index.svelte";

export type Tab = "host" | "join" | "mods" | "journal" | "network" | "look" | "settings";

export interface TabInfo {
  id: Tab;
  /** Label on the tab (dictionary key). */
  short: Key;
  /** Full section name: tab tooltip and screen reader text (dictionary key). */
  crumb: Key;
  /** Hotkey: Ctrl + digit. */
  key: string;
}

export const TABS: readonly TabInfo[] = [
  { id: "host", short: "nav.host", crumb: "nav.host", key: "1" },
  { id: "join", short: "nav.join", crumb: "nav.join", key: "2" },
  { id: "mods", short: "nav.mods", crumb: "nav.mods", key: "3" },
  { id: "journal", short: "nav.journal", crumb: "nav.journal", key: "4" },
  { id: "network", short: "nav.network", crumb: "nav.networkFull", key: "5" },
  // Room styling for players is second to last, window settings are last.
  { id: "look", short: "nav.look", crumb: "nav.lookFull", key: "6" },
  { id: "settings", short: "nav.settings", crumb: "nav.settings", key: "7" },
];

class NavStore {
  tab = $state<Tab>("host");

  get info(): TabInfo {
    return TABS.find((tab) => tab.id === this.tab) ?? TABS[0];
  }

  go(tab: Tab): void {
    this.tab = tab;
  }
}

export const nav = new NavStore();
