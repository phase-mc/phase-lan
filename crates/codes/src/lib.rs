//! Short Phase room codes: `K7F-2QX` instead of a long `endpoint…` ticket.
//!
//! The host registers a code with the code service (part of `phase-relay`) and renews it
//! every minute; the guest types the code and Phase looks up the ticket. **A code is not
//! access:** only the host lets people into the world ("Let in?" by device ID). The long
//! ticket stays as a fallback when the service is unavailable.
//!
//! Protocol (JSON):
//!
//! | Request | Body | Response |
//! |---|---|---|
//! | `POST /rooms` | [`RegisterRequest`] | `201` [`Registered`] |
//! | `PUT /rooms/{code}` | [`RenewRequest`] | `200` [`Renewed`]; `409`: someone else's code |
//! | `DELETE /rooms/{code}` | [`ReleaseRequest`] | `204`; `409`: someone else's code |
//! | `GET /rooms/{code}` | — | `200` [`Lookup`]; `404`: no such room |
//!
//! `429`: too many requests from one IP (`Retry-After` header).

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "pkarr")]
pub mod pkarr;

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Code characters: Crockford base32 without `0` and `1` (and, as in Crockford, without
/// `I L O U`): nothing that can be confused when dictating or copying from a screen.
pub const ALPHABET: &[u8; 30] = b"23456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Characters in a code (without the dash): 30⁶ ≈ 729 million combinations.
pub const CODE_LEN: usize = 6;
/// How long a code lives without renewal.
pub const TTL: Duration = Duration::from_secs(120);
/// How often the host renews the code.
pub const RENEW_EVERY: Duration = Duration::from_secs(60);
/// The longest ticket the service accepts (a normal one is about 100–200 characters).
pub const MAX_TICKET_LEN: usize = 2048;
/// Default code service port (next to the non-TLS relay on 3340).
pub const DEFAULT_PORT: u16 = 3341;
/// Code service address built into the program. `None` while the service is not deployed:
/// the address is set in the window ("Network" → "Code service") or `PHASE_CODES_URL`.
/// Without an address the window keeps codes in the public pkarr network (`pkarr` module), no server needed.
pub const DEFAULT_SERVICE_URL: Option<&str> = None;

/// Normalizes input to `K7F-2QX`: case, spaces and dashes do not matter.
/// `None` means this is not a room code (for example, a long ticket).
pub fn normalize(input: &str) -> Option<String> {
    let mut chars = Vec::with_capacity(CODE_LEN);
    for c in input.trim().chars() {
        if c == '-' || c.is_whitespace() {
            continue;
        }
        let c = c.to_ascii_uppercase();
        if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) || chars.len() == CODE_LEN {
            return None;
        }
        chars.push(c);
    }
    if chars.len() != CODE_LEN {
        return None;
    }
    let (a, b) = chars.split_at(CODE_LEN / 2);
    Some(format!(
        "{}-{}",
        a.iter().collect::<String>(),
        b.iter().collect::<String>()
    ))
}

/// The code is already in the exact `K7F-2QX` form (as the service stores and returns it).
pub fn is_code(code: &str) -> bool {
    normalize(code).as_deref() == Some(code)
}

/// Code from random bytes (one byte per character; `256 mod 30` gives a bias under one
/// percent; it does not matter for guessing, but biased bytes are discarded anyway).
/// `None` means there were not enough bytes.
pub fn code_from_random(bytes: &[u8]) -> Option<String> {
    let limit = (256 / ALPHABET.len() * ALPHABET.len()) as u8; // 240
    let picked: String = bytes
        .iter()
        .filter(|&&b| b < limit)
        .take(CODE_LEN)
        .map(|&b| ALPHABET[b as usize % ALPHABET.len()] as char)
        .collect();
    (picked.len() == CODE_LEN)
        .then(|| normalize(&picked))
        .flatten()
}

/// Whether it looks like an `EndpointTicket`: `endpoint` and lowercase base32. The guest
/// program checks the ticket in detail; the service only needs to avoid storing junk.
pub fn looks_like_ticket(ticket: &str) -> bool {
    ticket.len() <= MAX_TICKET_LEN
        && ticket.len() > "endpoint".len()
        && ticket.starts_with("endpoint")
        && ticket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisterRequest {
    pub ticket: String,
}

/// Code issued. Only the host knows `secret`: it uses it to renew and release the code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Registered {
    pub code: String,
    pub secret: String,
    pub ttl_secs: u64,
    pub renew_secs: u64,
}

/// Renewal. If the code has expired (the service restarted, the network was down), it is
/// created again with the same `secret`, provided another host has not taken it meanwhile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenewRequest {
    pub ticket: String,
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Renewed {
    pub ttl_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseRequest {
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lookup {
    pub ticket: String,
}

/// Error in a service response (`{"error": "..."}`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorBody {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_friendly_input() {
        assert_eq!(normalize("K7F-2QX").as_deref(), Some("K7F-2QX"));
        assert_eq!(normalize(" k7f2qx ").as_deref(), Some("K7F-2QX"));
        assert_eq!(normalize("k7f 2qx").as_deref(), Some("K7F-2QX"));
        assert!(is_code("K7F-2QX"));
        assert!(!is_code("k7f-2qx"));
    }

    #[test]
    fn normalize_rejects_lookalikes_and_tickets() {
        for bad in [
            "K0F-2QX",
            "K1F-2QX",
            "KOF-2QX",
            "KIF-2QX",
            "KLF-2QX",
            "K7F-2Q",
            "K7F-2QXX",
            "",
            "endpointabc",
        ] {
            assert_eq!(normalize(bad), None, "{bad}");
        }
    }

    #[test]
    fn random_codes_use_the_alphabet() {
        let bytes: Vec<u8> = (0..=255).collect();
        let code = code_from_random(&bytes).unwrap();
        assert!(is_code(&code), "{code}");
        // Only biased bytes: no code.
        assert_eq!(code_from_random(&[250; 16]), None);
    }

    #[test]
    fn tickets() {
        assert!(looks_like_ticket("endpointaa2b"));
        assert!(!looks_like_ticket("endpoint"));
        assert!(!looks_like_ticket("endpointAB"));
        assert!(!looks_like_ticket("hello"));
        assert!(!looks_like_ticket(&format!(
            "endpoint{}",
            "a".repeat(MAX_TICKET_LEN)
        )));
    }
}
