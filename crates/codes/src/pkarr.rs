//! Room codes without a server of our own: the "code → room" record lives in the public pkarr
//! network (the same `dns.iroh.link` server iroh uses to publish device addresses).
//!
//! An Ed25519 key is derived from the code (`blake3::derive_key`). The host signs with it
//! a record holding its room's address (device ID in `user_data`, relay and direct addresses
//! as in a normal iroh record) and refreshes it every [`RENEW_EVERY`](crate::RENEW_EVERY).
//! The guest derives the same public key from the code, reads the record, checks the
//! signature and freshness and builds a normal invite from it.
//!
//! **A code is not access.** Anyone who knows the code knows the key, so only someone who
//! guessed a live code (30⁶ ≈ 7·10⁸ combinations) can forge a record. Only the host lets
//! people into the world: "Let in?" by device ID.

use std::{str::FromStr, time::Duration};

use iroh_base::{EndpointAddr, EndpointId, SecretKey};
use iroh_dns::{
    endpoint_info::{EndpointData, EndpointInfo, UserData},
    pkarr::{SignedPacket, Timestamp},
};
use iroh_tickets::endpoint::EndpointTicket;
use phase_lang::tr;
use reqwest::StatusCode;

use crate::client::{CodesError, REQUEST_TIMEOUT, Result};

/// Default pkarr server (the same as iroh's).
pub const DEFAULT_RELAY: &str = "https://dns.iroh.link/pkarr";
/// Environment variable with a custom pkarr server (for tests).
pub const RELAY_ENV: &str = "PHASE_PKARR_URL";
/// A record older than this means the room is closed or the host is gone (the host refreshes every minute).
pub const FRESH_FOR: Duration = crate::TTL;
/// Record TTL in the DNS answer.
const DNS_TTL: u32 = 60;
/// Key derivation context: changes only together with the record format.
const KEY_CONTEXT: &str = "phase room code v1 2026-11-21";

/// Record key for code `K7F-2QX` (the code is already normalized).
pub fn code_key(code: &str) -> SecretKey {
    SecretKey::from_bytes(&blake3::derive_key(KEY_CONTEXT, code.as_bytes()))
}

/// Room code client over pkarr.
#[derive(Debug, Clone)]
pub struct PkarrCodes {
    base: String,
    http: reqwest::Client,
}

impl PkarrCodes {
    /// Server from [`RELAY_ENV`] or [`DEFAULT_RELAY`].
    pub fn from_env() -> Result<Self> {
        let base = std::env::var(RELAY_ENV)
            .ok()
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| DEFAULT_RELAY.to_string());
        Self::new(&base)
    }

    pub fn new(base: &str) -> Result<Self> {
        let base = base.trim().trim_end_matches('/');
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err(CodesError::Invalid(format!(
                "адрес pkarr «{base}» должен начинаться с http:// или https://"
            )));
        }
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("phase/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|err| CodesError::Invalid(format!("pkarr: {err}")))?;
        Ok(Self {
            base: base.to_string(),
            http,
        })
    }

    /// Host: a new random code for the invite `ticket`. Codes taken by a live room
    /// are skipped.
    pub async fn register(&self, ticket: &str) -> Result<String> {
        for _ in 0..4 {
            let code = random_code()?;
            match self.lookup(&code).await {
                Err(CodesError::NotFound) => {}
                Ok(_) => continue,
                Err(err) => return Err(err),
            }
            self.publish(&code, ticket).await?;
            return Ok(code);
        }
        Err(CodesError::Unavailable(
            tr!(
                "не удалось подобрать свободный код",
                "could not find a free code"
            )
            .into(),
        ))
    }

    /// Host: publish or renew the code record.
    pub async fn publish(&self, code: &str, ticket: &str) -> Result<()> {
        let ticket = EndpointTicket::from_str(ticket.trim())
            .map_err(|err| CodesError::Invalid(format!("приглашение: {err}")))?;
        let host = ticket.endpoint_addr();
        let key = code_key(code);
        let user_data = UserData::from_str(&host.id.to_string())
            .map_err(|err| CodesError::Invalid(format!("pkarr: {err}")))?;
        let info = EndpointInfo::from_parts(key.public(), EndpointData::from(host.addrs.clone()))
            .with_user_data(Some(user_data));
        self.put(&key, &info).await
    }

    /// Host: release the code (an empty record without a room).
    pub async fn release(&self, code: &str) -> Result<()> {
        let key = code_key(code);
        self.put(&key, &EndpointInfo::new(key.public())).await
    }

    /// Guest: code → invite. [`CodesError::NotFound`] means no live room.
    pub async fn lookup(&self, code: &str) -> Result<String> {
        let public = code_key(code).public();
        let response = self
            .http
            .get(format!("{}/{}", self.base, public.to_z32()))
            .send()
            .await
            .map_err(|err| CodesError::Unavailable(err.to_string()))?;
        match response.status() {
            StatusCode::NOT_FOUND => return Err(CodesError::NotFound),
            StatusCode::TOO_MANY_REQUESTS => return Err(CodesError::RateLimited),
            status if !status.is_success() => {
                return Err(CodesError::Unavailable(format!("HTTP {status}")));
            }
            _ => {}
        }
        let payload = response
            .bytes()
            .await
            .map_err(|err| CodesError::Unavailable(err.to_string()))?;
        let packet = SignedPacket::from_relay_payload(&public, &payload)
            .map_err(|err| CodesError::Unavailable(format!("pkarr: {err}")))?;
        ticket_from_packet(&packet, Timestamp::now())
    }

    async fn put(&self, key: &SecretKey, info: &EndpointInfo) -> Result<()> {
        let packet = info
            .to_pkarr_signed_packet(key, DNS_TTL)
            .map_err(|err| CodesError::Invalid(format!("pkarr: {err}")))?;
        let response = self
            .http
            .put(format!("{}/{}", self.base, key.public().to_z32()))
            .body(packet.to_relay_payload())
            .send()
            .await
            .map_err(|err| CodesError::Unavailable(err.to_string()))?;
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::TOO_MANY_REQUESTS => Err(CodesError::RateLimited),
            status => Err(CodesError::Unavailable(format!("HTTP {status}"))),
        }
    }
}

/// Invite from a signed record; an old or empty record gives [`CodesError::NotFound`].
pub fn ticket_from_packet(packet: &SignedPacket, now: Timestamp) -> Result<String> {
    let age = now
        .as_micros()
        .saturating_sub(packet.timestamp().as_micros());
    if Duration::from_micros(age) > FRESH_FOR {
        return Err(CodesError::NotFound);
    }
    let info = EndpointInfo::from_pkarr_signed_packet(packet)
        .map_err(|err| CodesError::Unavailable(format!("pkarr: {err}")))?;
    let Some(host) = info
        .user_data()
        .and_then(|data| EndpointId::from_str(data.as_ref()).ok())
    else {
        return Err(CodesError::NotFound);
    };
    let addrs = info.into_endpoint_addr().addrs;
    if addrs.is_empty() {
        return Err(CodesError::NotFound);
    }
    Ok(EndpointTicket::new(EndpointAddr { id: host, addrs }).to_string())
}

fn random_code() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|err| CodesError::Invalid(format!("getrandom: {err}")))?;
    crate::code_from_random(&bytes).ok_or_else(|| CodesError::Invalid("getrandom".into()))
}

#[cfg(test)]
mod tests {
    use iroh_base::{RelayUrl, TransportAddr};

    use super::*;

    fn host_ticket() -> (EndpointId, String) {
        let host = SecretKey::from_bytes(&[7; 32]).public();
        let relay: RelayUrl = "https://relay.example.com".parse().unwrap();
        let addr = EndpointAddr::from_parts(
            host,
            [
                TransportAddr::Relay(relay),
                TransportAddr::Ip("203.0.113.5:4433".parse().unwrap()),
            ],
        );
        (host, EndpointTicket::new(addr).to_string())
    }

    fn packet_for(code: &str, ticket: &str) -> SignedPacket {
        let host = EndpointTicket::from_str(ticket)
            .unwrap()
            .endpoint_addr()
            .clone();
        let key = code_key(code);
        EndpointInfo::from_parts(key.public(), EndpointData::from(host.addrs))
            .with_user_data(Some(UserData::from_str(&host.id.to_string()).unwrap()))
            .to_pkarr_signed_packet(&key, DNS_TTL)
            .unwrap()
    }

    #[test]
    fn same_code_same_key_other_code_other_key() {
        assert_eq!(code_key("K7F-2QX").public(), code_key("K7F-2QX").public());
        assert_ne!(code_key("K7F-2QX").public(), code_key("K7F-2QY").public());
    }

    #[test]
    fn record_round_trips_to_the_same_invite() {
        let (host, ticket) = host_ticket();
        let packet = packet_for("K7F-2QX", &ticket);
        // As on the guest: check the signature with the public key from the code.
        let packet = SignedPacket::from_relay_payload(
            &code_key("K7F-2QX").public(),
            &packet.to_relay_payload(),
        )
        .unwrap();
        let back = ticket_from_packet(&packet, Timestamp::now()).unwrap();
        let addr = EndpointTicket::from_str(&back)
            .unwrap()
            .endpoint_addr()
            .clone();
        assert_eq!(addr.id, host);
        assert_eq!(
            addr,
            EndpointTicket::from_str(&ticket)
                .unwrap()
                .endpoint_addr()
                .clone()
        );
    }

    #[test]
    fn record_signed_for_another_code_is_rejected() {
        let (_, ticket) = host_ticket();
        let packet = packet_for("AAA-AAA", &ticket);
        assert!(
            SignedPacket::from_relay_payload(
                &code_key("K7F-2QX").public(),
                &packet.to_relay_payload()
            )
            .is_err()
        );
    }

    #[test]
    fn stale_or_empty_record_means_no_room() {
        let (_, ticket) = host_ticket();
        let packet = packet_for("K7F-2QX", &ticket);
        let later = Timestamp::from_micros(
            packet.timestamp().as_micros()
                + (FRESH_FOR + Duration::from_secs(1)).as_micros() as u64,
        );
        assert!(matches!(
            ticket_from_packet(&packet, later),
            Err(CodesError::NotFound)
        ));

        let key = code_key("K7F-2QX");
        let empty = EndpointInfo::new(key.public())
            .to_pkarr_signed_packet(&key, DNS_TTL)
            .unwrap();
        assert!(matches!(
            ticket_from_packet(&empty, Timestamp::now()),
            Err(CodesError::NotFound)
        ));
    }
}
