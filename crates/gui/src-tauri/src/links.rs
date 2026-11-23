//! `phase://` links: an invitation to a room from a browser, messenger or the website.
//!
//! Link path: Windows launches `Phase.exe "phase://…"` → the single-instance plugin
//! passes the string to the already open window (or the window starts with it) → the deep-link
//! plugin → [`open`] parses the link → [`LinkInbox`] holds it until the window → the
//! [`LINK_CHANNEL`] event wakes the window, which picks the link up with the `take_link` command.
//!
//! **The program never connects anywhere by itself.** Anyone could have sent the link, and a direct
//! connection reveals the IP address. The window only opens "Join" with the
//! code filled in; the user decides by pressing "Join".
//!
//! Grammar (everything else is discarded):
//!
//! | Link | Meaning |
//! |---|---|
//! | `phase://join/K7F-2QX` | random room code (Crockford base32, 6 characters) |
//! | `phase://join/vanilla-crew` | Pro named code (3–16 characters `a–z 0–9 -`) |
//! | `phase://join?t=<ticket>` | the whole host ticket (fallback without the code service) |
//!
//! `phase://auth/callback` and `phase://pro/activated` will arrive with accounts (stage 9.4).

use std::{str::FromStr, sync::Mutex};

use phase_tunnel::EndpointTicket;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use url::Url;

/// Link scheme (same as in `tauri.conf.json` → `plugins.deep-link`).
pub const SCHEME: &str = "phase";
/// Event for the window: a link arrived, pick it up with the `take_link` command.
pub const LINK_CHANNEL: &str = "phase://link";
/// No link is ever longer: anything longer is discarded unparsed.
const MAX_URL_LEN: usize = 2048;
/// No ticket is ever longer (a normal one is about 100–200 characters).
const MAX_TICKET_LEN: usize = 1024;

/// What the link asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Link {
    /// Room code: fill it in on the "Join" tab.
    Join { code: String },
    /// Host ticket: fill it in on the "Join" tab.
    Ticket { ticket: String },
}

/// The latest link that arrived, until the window picks it up.
#[derive(Default)]
pub struct LinkInbox(Mutex<Option<Link>>);

impl LinkInbox {
    pub fn put(&self, link: Link) {
        *self.0.lock().expect("link inbox lock") = Some(link);
    }

    pub fn take(&self) -> Option<Link> {
        self.0.lock().expect("link inbox lock").take()
    }
}

/// Parses a link. `None` — not our link, corrupted, or not supported yet.
pub fn parse(text: &str) -> Option<Link> {
    if text.len() > MAX_URL_LEN {
        return None;
    }
    // URL parsing collapses `.` and `..` (`join/../../etc` → `/etc`): such links
    // are rejected before parsing so the window gets exactly what was written.
    if text
        .split(['/', '\\', '?', '#'])
        .any(|segment| segment == "." || segment == ".." || segment.eq_ignore_ascii_case("%2e"))
        || text.to_ascii_lowercase().contains("%2e%2e")
    {
        return None;
    }
    let url = Url::parse(text).ok()?;
    if url.scheme() != SCHEME
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    // For a non-standard scheme `url` does not lowercase the host — compare case-insensitively.
    if !url.host_str()?.eq_ignore_ascii_case("join") {
        return None;
    }
    let path = url.path().trim_start_matches('/').trim_end_matches('/');
    let ticket = url.query_pairs().find(|(key, _)| key == "t");
    match (path.is_empty(), ticket) {
        (false, None) => is_room_code(path).then(|| Link::Join {
            code: path.to_string(),
        }),
        (true, Some((_, ticket))) => {
            let ticket = ticket.trim();
            (ticket.len() <= MAX_TICKET_LEN && EndpointTicket::from_str(ticket).is_ok()).then(
                || Link::Ticket {
                    ticket: ticket.to_string(),
                },
            )
        }
        _ => None,
    }
}

/// Room code: a random `K7F-2QX` or a named `vanilla-crew`.
pub fn is_room_code(code: &str) -> bool {
    is_random_code(code) || is_vanity_code(code)
}

/// Random code: 6 uppercase characters with a hyphen (alphabet — `phase_codes::ALPHABET`:
/// Crockford base32 without 0 and 1).
fn is_random_code(code: &str) -> bool {
    phase_codes::is_code(code)
}

/// Named Pro code: 3–16 characters `a–z 0–9 -`, no hyphen at the ends and no two in a row.
fn is_vanity_code(code: &str) -> bool {
    (3..=16).contains(&code.len())
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !code.starts_with('-')
        && !code.ends_with('-')
        && !code.contains("--")
}

/// Links from the system: parse, store for the window, wake it and show the window.
pub fn open<R: Runtime>(app: &AppHandle<R>, urls: impl IntoIterator<Item = Url>) {
    // Of several links at once, take the last suitable one.
    let Some(link) = urls
        .into_iter()
        .filter_map(|url| {
            let link = parse(url.as_str());
            if link.is_none() {
                tracing::warn!("ссылка {SCHEME}:// не распознана и пропущена");
            }
            link
        })
        .last()
    else {
        return;
    };
    tracing::info!("пришла ссылка-приглашение: {}", describe(&link));
    app.state::<LinkInbox>().put(link.clone());
    let _ = app.emit(LINK_CHANNEL, &link);
    show_main_window(app);
}

/// Bring the window to the front (a minimized one is restored).
pub fn show_main_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// For the log: the whole ticket is not written, it is long and reveals the host's address.
fn describe(link: &Link) -> String {
    match link {
        Link::Join { code } => format!("код комнаты {code}"),
        Link::Ticket { .. } => "приглашение хоста".to_string(),
    }
}

/// Registers the scheme in Windows for the portable build (no installer): key
/// `HKCU\Software\Classes\phase`, no administrator rights needed. Written if the scheme is not
/// registered or points to a different exe (the program was moved to another folder).
#[cfg(windows)]
pub fn ensure_registered<R: Runtime>(app: &AppHandle<R>) {
    use tauri_plugin_deep_link::DeepLinkExt;
    // A debug build (`tauri dev`, checks) does not take links from the installed
    // program — only on explicit request.
    if cfg!(debug_assertions) && std::env::var_os("PHASE_REGISTER_SCHEME").is_none_or(|v| v != "1")
    {
        return;
    }
    let deep_link = app.deep_link();
    match deep_link.is_registered(SCHEME) {
        Ok(true) => {}
        _ => match deep_link.register_all() {
            Ok(()) => tracing::info!("ссылки {SCHEME}:// теперь открываются в этой программе"),
            Err(err) => tracing::warn!("не удалось зарегистрировать ссылки {SCHEME}://: {err}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn join(code: &str) -> Option<Link> {
        Some(Link::Join { code: code.into() })
    }

    #[test]
    fn room_codes() {
        assert_eq!(parse("phase://join/K7F-2QX"), join("K7F-2QX"));
        assert_eq!(parse("phase://join/K7F-2QX/"), join("K7F-2QX"));
        assert_eq!(parse("phase://join/vanilla-crew"), join("vanilla-crew"));
        // The browser may change the case of the scheme and host — it's the same link.
        assert_eq!(parse("PHASE://JOIN/K7F-2QX"), join("K7F-2QX"));
    }

    #[test]
    fn bad_codes_are_dropped() {
        for text in [
            "phase://join/K7F-2QI",                 // no I in Crockford base32
            "phase://join/K7F-2Q0",                 // 0 and 1 are not part of codes
            "phase://join/ab",                      // shorter than 3
            "phase://join/a-very-long-room-name-x", // longer than 16
            "phase://join/-crew",                   // hyphen at the edge
            "phase://join/van--crew",               // two hyphens in a row
            "phase://join/Vanilla-Crew",            // named — lowercase only
            "phase://join/k7f%2F2qx",               // encoded slash
            "phase://join/a/b",                     // extra segment
            "phase://join/../../etc",               // the dots would collapse into "etc"
            "phase://join/x/../crew",
            "phase://join/%2e%2e/crew",
            "phase://join/./crew",
            "phase://join/",             // empty
            "phase://join/K7F-2QX?t=x",  // both a code and a ticket
            "phase://user@join/K7F-2QX", // extra address parts
            "phase://join:80/K7F-2QX",
            "https://join/K7F-2QX",                 // foreign scheme
            "phase://auth/callback?code=1&state=2", // not supported yet
            "phase://pro/activated",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn tickets_pass_the_same_checks_as_pasted_ones() {
        use phase_tunnel::{EndpointAddr, SecretKey};
        let addr = EndpointAddr::from_parts(SecretKey::generate().public(), []);
        let ticket = EndpointTicket::new(addr).to_string();
        assert_eq!(
            parse(&format!("phase://join?t={ticket}")),
            Some(Link::Ticket {
                ticket: ticket.clone()
            })
        );
        assert_eq!(parse("phase://join?t=endpointnotaticket"), None);
        assert_eq!(parse("phase://join?t="), None);
        assert_eq!(parse("phase://join"), None);
    }

    #[test]
    fn long_links_are_dropped_unparsed() {
        let long = format!("phase://join?t={}", "a".repeat(MAX_URL_LEN));
        assert_eq!(parse(&long), None);
    }
}
