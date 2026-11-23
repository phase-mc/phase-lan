//! Creating an iroh endpoint with our settings.

use phase_lang::tr;
use std::time::Duration;

use std::path::Path;

use anyhow::{Context, Result, bail};
use iroh::{
    Endpoint, EndpointAddr, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, Watcher,
    endpoint::{QuicTransportConfig, default_relay_mode, presets},
    tls::CaTlsConfig,
};
use rustls_pki_types::{CertificateDer, pem::PemObject};

use crate::{access::AccessHook, wire};

/// How long a QUIC connection lives without connectivity before closing.
/// The value is the same as iroh's default, but set explicitly: both sides must
/// have the same one (the smaller of the two is used).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// Check of the connection as a whole.
pub const KEEP_ALIVE: Duration = Duration::from_secs(5);
/// Check of each path (direct and via relay): more often, to notice a drop quickly.
pub const PATH_KEEP_ALIVE: Duration = Duration::from_secs(1);
/// A path without an answer for longer than this is considered dead, and traffic moves to another path.
/// Goal: the total pause on a switch stays under 10 s (the game disconnects after about 15 s).
pub const PATH_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
/// A small datagram buffer (voice): old packets are better dropped than queued.
pub const DATAGRAM_SEND_BUFFER: usize = 64 * 1024;
/// How many streams the other side can open at once. Each game connection is
/// one stream; Minecraft opens 1–2 per player. Without a limit, a stranger waiting for
/// approval could take hundreds of megabytes of the host's memory.
pub const MAX_STREAMS: u32 = 32;
/// Total in-flight data per connection. Plenty for a game.
pub const RECEIVE_WINDOW: u32 = 8 * 1024 * 1024;

/// Which network paths are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkMode {
    /// Normal mode: direct paths (UDP on all interfaces) and relay.
    #[default]
    Normal,
    /// Relay only (test T4). Direct paths are off.
    RelayOnly,
    /// `127.0.0.1` only, without relay and internet. For checks on one PC
    /// without a firewall prompt and for automated tests.
    LoopbackOnly,
}

pub struct EndpointOptions {
    pub secret_key: SecretKey,
    /// Accept incoming connections (the host needs this).
    pub accept_incoming: bool,
    pub network: NetworkMode,
    /// Which relay servers to use. Not used in `LoopbackOnly` mode.
    pub relay: RelaySettings,
    /// Admission check for incoming connections (the host needs this).
    pub access: Option<AccessHook>,
}

/// Relay servers and access to them.
///
/// By default (empty list) the free n0 relays: **for development only**.
/// Our own relay is the `phase-relay` program.
#[derive(Debug, Clone, Default)]
pub struct RelaySettings {
    /// Our own relay servers (`https://…`, `http://…` for home network checks).
    pub urls: Vec<RelayUrl>,
    /// Access password for our own relay, if the server requires one.
    pub token: Option<String>,
    /// Extra trusted certificates: for a relay with a self-signed certificate.
    pub extra_roots: Vec<CertificateDer<'static>>,
    /// The host's own relays from the ticket (filled by [`RelaySettings::with_relays_of`]).
    pub peer_urls: Vec<RelayUrl>,
}

impl RelaySettings {
    /// The guest adds the host's own relays from the ticket: so all traffic goes through the host's
    /// relay (not through n0 relays), and the password (`--relay-token`) applies to it.
    /// n0 relays from the ticket are not added: they are used by default anyway.
    pub fn with_relays_of(mut self, host: &EndpointAddr) -> Self {
        self.peer_urls = host
            .relay_urls()
            .filter(|url| !is_n0_relay(url) && !self.urls.contains(url))
            .cloned()
            .collect();
        self
    }

    /// Whether our own relays are used (not n0 relays).
    pub fn is_custom(&self) -> bool {
        !self.urls.is_empty() || !self.peer_urls.is_empty()
    }

    /// All our own relays: explicitly given ones first, then the host's relays.
    pub fn all_urls(&self) -> impl Iterator<Item = &RelayUrl> {
        self.urls.iter().chain(&self.peer_urls)
    }

    /// Warnings about settings that work, but worse than they could.
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        for url in self.all_urls().filter(|u| u.scheme() == "http") {
            out.push(tr!(
                format!(
                    "relay {url} работает без TLS (http://): это подходит для домашней сети и \
                     проверок. Через интернет пароль relay передаётся открыто, а прямое соединение \
                     удаётся реже (без TLS relay не помогает узнать внешний адрес)"
                ),
                format!(
                    "relay {url} runs without TLS (http://): fine for a home network and \
                     testing. Over the internet the relay password is sent in the clear, and \
                     direct connections succeed less often (without TLS the relay cannot help \
                     find your public address)"
                )
            ));
        }
        if self.token.is_some() && !self.is_custom() {
            out.push(
                tr!(
                    "пароль relay задан, но свой relay не указан (--relay) — пароль не используется",
                    "a relay password is set but no custom relay is given (--relay): the password is not used"
                )
                .to_string(),
            );
        }
        out
    }

    /// Relay mode for iroh.
    pub fn mode(&self) -> RelayMode {
        if !self.is_custom() {
            return default_relay_mode();
        }
        RelayMode::Custom(
            self.all_urls()
                .map(|url| relay_config(url, self.token_for(url)))
                .collect::<RelayMap>(),
        )
    }

    /// The password is given only to "our" relays: explicitly given ones, the host's relays if the guest
    /// gave none of its own, or a host relay on the same server (same name and port) as an
    /// explicitly given one. So the password for one relay does not leak to another relay's owner.
    fn token_for(&self, url: &RelayUrl) -> Option<&str> {
        let token = self.token.as_deref()?;
        let own = self.urls.contains(url)
            || self.urls.is_empty()
            || self.urls.iter().any(|mine| same_server(mine, url));
        own.then_some(token)
    }
}

/// Settings of one relay. QUIC address discovery (learning our external address) exists
/// only on TLS relays: we do not look for it on `http://` relays, to avoid waiting for nothing.
fn relay_config(url: &RelayUrl, token: Option<&str>) -> RelayConfig {
    let config = if url.scheme() == "https" {
        RelayConfig::from(url.clone())
    } else {
        RelayConfig::new(url.clone(), None)
    };
    match token {
        Some(token) => config.with_auth_token(token),
        None => config,
    }
}

fn same_server(a: &RelayUrl, b: &RelayUrl) -> bool {
    let host = |u: &RelayUrl| {
        u.host_str()
            .map(|h| h.trim_end_matches('.').to_ascii_lowercase())
    };
    a.scheme() == b.scheme()
        && host(a) == host(b)
        && a.port_or_known_default() == b.port_or_known_default()
}

/// Free n0 relay (the main or the staging set).
pub fn is_n0_relay(url: &RelayUrl) -> bool {
    RelayMode::Default.relay_map().contains(url) || RelayMode::Staging.relay_map().contains(url)
}

/// Checks a relay address: only `https://` (or `http://` for home networks and checks),
/// without a login, password or parameters in the address.
pub fn parse_relay_url(text: &str) -> Result<RelayUrl> {
    let url: RelayUrl = text.trim().parse().map_err(|_| {
        anyhow::anyhow!(tr!(
            format!("«{text}» — не адрес relay (пример: https://relay.example.com)"),
            format!("\"{text}\" is not a relay address (example: https://relay.example.com)")
        ))
    })?;
    if !matches!(url.scheme(), "https" | "http") {
        bail!(tr!(
            format!(
                "адрес relay должен начинаться с https:// (или http:// для домашней сети), а не {}://",
                url.scheme()
            ),
            format!(
                "the relay address must start with https:// (or http:// on a home network), not {}://",
                url.scheme()
            )
        ));
    }
    if url.host_str().is_none_or(str::is_empty) {
        bail!(tr!(
            format!("в адресе relay «{text}» нет имени сервера"),
            format!("the relay address \"{text}\" has no server name")
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        bail!(tr!(
            "в адресе relay не должно быть логина, пароля или параметров; пароль relay задаётся отдельно: --relay-token",
            "the relay address must not contain a username, password or parameters; the relay password is set separately: --relay-token"
        ));
    }
    Ok(url)
}

/// Reads trusted certificates (PEM) for a relay with a self-signed certificate.
pub fn read_ca_file(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .with_context(|| {
            tr!(
                format!("не удалось открыть сертификат relay {}", path.display()),
                format!("could not open relay certificate {}", path.display())
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| {
            tr!(
                format!("не удалось прочитать сертификат relay {}", path.display()),
                format!("could not read relay certificate {}", path.display())
            )
        })?;
    if certs.is_empty() {
        bail!(tr!(
            format!(
                "в файле {} нет сертификатов (нужен формат PEM)",
                path.display()
            ),
            format!(
                "{} contains no certificates (PEM format required)",
                path.display()
            )
        ));
    }
    Ok(certs)
}

pub fn transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .max_idle_timeout(Some(
            IDLE_TIMEOUT
                .try_into()
                .expect("30 seconds fit in IdleTimeout"),
        ))
        .keep_alive_interval(KEEP_ALIVE)
        .default_path_keep_alive_interval(PATH_KEEP_ALIVE)
        .default_path_max_idle_timeout(PATH_IDLE_TIMEOUT)
        .datagram_send_buffer_size(DATAGRAM_SEND_BUFFER)
        // The protocol does not use unidirectional streams.
        .max_concurrent_uni_streams(0u32.into())
        .max_concurrent_bidi_streams(MAX_STREAMS.into())
        .receive_window(RECEIVE_WINDOW.into())
        .build()
}

/// Creates an iroh endpoint.
///
/// The `Minimal` preset (crypto only) is used, not `N0`: we do not publish our
/// addresses in the n0 DNS service. The guest gets the host's address from the ticket.
///
/// Relay: our own servers from [`RelaySettings`], and if none are set, the free n0 relays
/// (**for development and tests only**).
pub async fn bind(opts: EndpointOptions) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(opts.secret_key)
        .transport_config(transport_config());
    if !opts.relay.extra_roots.is_empty() {
        // The usual root certificates stay, our own relay's certificate is added.
        builder = builder.ca_tls_config(
            CaTlsConfig::embedded().with_extra_roots(opts.relay.extra_roots.iter().cloned()),
        );
    }

    builder = match opts.network {
        NetworkMode::Normal => builder.relay_mode(opts.relay.mode()),
        NetworkMode::RelayOnly => builder.relay_mode(opts.relay.mode()).clear_ip_transports(),
        NetworkMode::LoopbackOnly => builder
            .relay_mode(RelayMode::Disabled)
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0")
            .context(tr!(
                "не удалось задать адрес 127.0.0.1",
                "could not bind to 127.0.0.1"
            ))?,
    };
    if opts.accept_incoming {
        builder = builder.alpns(vec![wire::ALPN.to_vec()]);
    }
    if let Some(access) = opts.access {
        builder = builder.hooks(access);
    }
    builder.bind().await.context(tr!(
        "не удалось запустить сеть Phase",
        "could not start the Phase network"
    ))
}

/// Endpoint address for the ticket.
///
/// In normal mode wait for the relay connection (up to `wait`) so the relay address gets into
/// the ticket. In `LoopbackOnly` mode there is no relay, so take the actually open sockets.
pub async fn shareable_addr(
    endpoint: &Endpoint,
    network: NetworkMode,
    wait: Duration,
) -> EndpointAddr {
    match network {
        NetworkMode::LoopbackOnly => EndpointAddr::from_parts(
            endpoint.id(),
            endpoint
                .bound_sockets()
                .into_iter()
                .map(iroh::TransportAddr::Ip),
        ),
        NetworkMode::Normal | NetworkMode::RelayOnly => {
            if tokio::time::timeout(wait, endpoint.online()).await.is_err() {
                let why = relay_problem(endpoint).unwrap_or_else(|| {
                    tr!(
                        "нет связи с relay: сервер недоступен, адрес неверный или сертификат \
                         relay не доверенный (для самоподписанного нужен --relay-ca)",
                        "cannot reach the relay: the server is down, the address is wrong or the \
                         relay certificate is not trusted (a self-signed one needs --relay-ca)"
                    )
                    .into()
                });
                tracing::warn!(
                    "{why} (ждал {} с): приглашение будет содержать только прямые адреса, \
                     друзья за строгим NAT не подключатся",
                    wait.as_secs()
                );
            }
            endpoint.addr()
        }
    }
}

/// A hint for a connection error when our own relay is used and there is no connection to it.
pub fn relay_hint(
    endpoint: &Endpoint,
    relay: &RelaySettings,
    network: NetworkMode,
) -> Option<String> {
    if network == NetworkMode::LoopbackOnly || !relay.is_custom() {
        return None;
    }
    if let Some(problem) = relay_problem(endpoint) {
        return Some(problem);
    }
    let online = endpoint
        .home_relay_status()
        .get()
        .iter()
        .any(|s| s.is_connected());
    (!online).then(|| {
        tr!(
            "нет связи со своим relay: сервер недоступен, адрес неверный или сертификат relay \
             не доверенный (для самоподписанного нужен --relay-ca)",
            "cannot reach your relay: the server is down, the address is wrong or the relay \
             certificate is not trusted (a self-signed one needs --relay-ca)"
        )
        .to_string()
    })
}

/// Why there is no connection to the relay (if there is none): access denied (wrong password) or
/// a connection error. `None` means the relay is connected.
pub fn relay_problem(endpoint: &Endpoint) -> Option<String> {
    let statuses = endpoint.home_relay_status().get();
    if statuses.iter().any(|status| status.is_connected()) {
        return None;
    }
    if let Some((url, reason)) = statuses
        .iter()
        .find_map(|s| s.auth_denied_reason().map(|r| (s.url(), r)))
    {
        let reason = wire::sanitize_text(reason, 200);
        return Some(tr!(
            format!(
                "relay {url} отказал в доступе ({reason}): проверьте пароль relay (--relay-token) или \
                 попросите владельца relay добавить ваше устройство"
            ),
            format!(
                "relay {url} denied access ({reason}): check the relay password (--relay-token) or \
                 ask the relay owner to add your device"
            )
        ));
    }
    statuses.iter().find_map(|status| {
        status.last_error().map(|err| {
            let detail = wire::sanitize_text(&format!("{err:#}"), 300);
            tr!(
                format!("нет связи с relay {}: {detail}", status.url()),
                format!("cannot reach relay {}: {detail}", status.url())
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use iroh::TransportAddr;

    use super::*;

    fn url(s: &str) -> RelayUrl {
        parse_relay_url(s).unwrap()
    }

    fn token_of(map: &RelayMap, u: &RelayUrl) -> Option<String> {
        map.get(u).and_then(|c| c.auth_token.clone())
    }

    #[test]
    fn relay_urls_are_checked() {
        assert!(parse_relay_url("https://relay.example.com").is_ok());
        assert!(parse_relay_url("http://192.168.1.10:3340").is_ok());
        assert!(parse_relay_url("ftp://relay.example.com").is_err());
        assert!(parse_relay_url("relay.example.com").is_err());
        assert!(parse_relay_url("https://user:pass@relay.example.com").is_err());
        assert!(parse_relay_url("https://relay.example.com/?token=abc").is_err());
    }

    #[test]
    fn default_is_n0_and_custom_replaces_it() {
        let settings = RelaySettings::default();
        assert!(!settings.is_custom());
        assert_eq!(settings.mode(), default_relay_mode());

        let mine = url("https://relay.example.com");
        let settings = RelaySettings {
            urls: vec![mine.clone()],
            token: Some("secret-token-123456".into()),
            ..Default::default()
        };
        let RelayMode::Custom(map) = settings.mode() else {
            panic!("our own relay should replace the n0 relays");
        };
        assert_eq!(map.urls::<Vec<_>>(), vec![mine.clone()]);
        assert_eq!(
            token_of(&map, &mine).as_deref(),
            Some("secret-token-123456")
        );
        // TLS relays get QUIC address discovery, http relays do not.
        assert!(map.get(&mine).unwrap().quic.is_some());
        let plain = url("http://127.0.0.1:3340");
        assert!(relay_config(&plain, None).quic.is_none());
    }

    #[test]
    fn guest_takes_host_relay_from_ticket() {
        let host_relay = url("https://relay.friend.example");
        let n0 = RelayMode::Default.relay_map().urls::<Vec<_>>()[0].clone();
        let host = EndpointAddr::from_parts(
            SecretKey::generate().public(),
            [
                TransportAddr::Relay(host_relay.clone()),
                TransportAddr::Relay(n0.clone()),
            ],
        );

        // A guest without its own relays: the host's relays (except n0) plus the password for them.
        let settings = RelaySettings {
            token: Some("friend-token-123456".into()),
            ..Default::default()
        }
        .with_relays_of(&host);
        assert_eq!(settings.peer_urls, vec![host_relay.clone()]);
        let RelayMode::Custom(map) = settings.mode() else {
            panic!("the host's relay should be used");
        };
        assert!(!map.contains(&n0));
        assert_eq!(
            token_of(&map, &host_relay).as_deref(),
            Some("friend-token-123456")
        );

        // Host on n0 relays: nothing changes for the guest.
        let n0_host =
            EndpointAddr::from_parts(SecretKey::generate().public(), [TransportAddr::Relay(n0)]);
        let settings = RelaySettings::default().with_relays_of(&n0_host);
        assert!(!settings.is_custom());
    }

    #[test]
    fn token_is_not_sent_to_someone_elses_relay() {
        let mine = url("https://my-relay.example");
        let same_server = url("https://MY-RELAY.example.:443/");
        let other = url("https://other-relay.example");
        let settings = RelaySettings {
            urls: vec![mine.clone()],
            token: Some("my-secret-token-1234".into()),
            peer_urls: vec![same_server.clone(), other.clone()],
            ..Default::default()
        };
        let RelayMode::Custom(map) = settings.mode() else {
            panic!("our own relays");
        };
        assert_eq!(map.len(), 3);
        assert!(token_of(&map, &mine).is_some());
        assert!(
            token_of(&map, &same_server).is_some(),
            "same server, different spelling"
        );
        assert!(
            token_of(&map, &other).is_none(),
            "the password does not go to someone else's relay"
        );
    }
}
