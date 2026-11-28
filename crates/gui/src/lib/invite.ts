// Invite links for chats: `https://phase.yeskendir.com/join/#K7F-2QX`, one link for every language.
// A chat's preview shows the English page; a browser goes on to the visitor's language
// (/en/, /ru/ or /tr/join/) with the code, and the page opens `phase://join/K7F-2QX`
// (Rust: links.rs) or offers to download Phase LAN. The code sits in the fragment: browsers never
// send it to the server, so it stays out of request logs and out of the request a messenger makes
// for the link preview.
//
// Only short codes go into links. A code dies a couple of minutes after the room closes, so a
// forwarded link stops working by itself; the long invite carries the device's permanent address
// and stays out of links on purpose.

import { normalizeCode } from "./core";

/** The Phase site (site/src/lib/site-map.mjs → SITE). */
export const SITE_ORIGIN = "https://phase.yeskendir.com";

/** Invite link for a room code; `null` — not a room code. */
export function inviteUrl(code: string): string | null {
  const normalized = normalizeCode(code);
  return normalized ? `${SITE_ORIGIN}/join/#${normalized}` : null;
}

/** The page path: `/join/` or `/<lang>/join/` (with or without the last slash). */
const PAGE_PATH = /^\/(?:(?:en|ru|tr)\/)?join\/?$/;
/** `phase://join/<code>` as the site page opens it (any case, an optional last slash). */
const APP_LINK = /^phase:\/\/join\/([^/?#]+)\/?$/i;

/** Room code from what the guest pasted into "Join": the code itself, an invite link from the
 *  site or a `phase://join/…` link. `null` — none of these (for example, a long invite). */
export function codeFromInput(text: string): string | null {
  const input = text.trim();
  const plain = normalizeCode(input);
  if (plain) return plain;
  const app = APP_LINK.exec(input);
  if (app) return normalizeCode(decode(app[1]));
  let url: URL;
  try {
    url = new URL(input);
  } catch {
    return null;
  }
  if (url.origin !== SITE_ORIGIN || !PAGE_PATH.test(url.pathname) || url.search) return null;
  return normalizeCode(decode(url.hash.slice(1)));
}

function decode(text: string): string {
  try {
    return decodeURIComponent(text);
  } catch {
    return "";
  }
}
