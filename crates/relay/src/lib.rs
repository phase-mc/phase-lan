//! Phase relay: our own relay server (based on iroh-relay from the iroh authors).
//!
//! A relay is needed when players cannot connect directly (strict NAT, mobile
//! internet): packets go through the server. The relay cannot read them: encryption is
//! device to device (QUIC/TLS 1.3). The relay sees only device IDs,
//! their IP addresses and the traffic volume.
//!
//! Modes:
//! - **HTTP without TLS**: for LAN testing and automated tests;
//! - **HTTPS**: for the internet, with a Let's Encrypt certificate, ready files or
//!   a self-signed one (see [`certs`]). QUIC address discovery also runs
//!   (clients learn their external address, which helps direct connections).
//!
//! Access can be restricted by password and/or device list (see [`access`]).
//!
//! A room code service can run next to the relay (see [`codes`]): `K7F-2QX` → ticket.

pub mod access;
pub mod certs;
pub mod codes;

use std::{net::SocketAddr, num::NonZeroU32, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use iroh_base::{EndpointId, RelayUrl};
use iroh_relay::server::{
    ClientRateLimit, QuicConfig, RelayConfig, Server, ServerConfig, TlsConfig,
};
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub use access::{AccessRules, RelayAccess};
pub use certs::CertSource;
pub use codes::{CodesOptions, CodesServer};

/// Default HTTP port in non-TLS mode (as with `iroh-relay --dev`).
pub const DEFAULT_DEV_HTTP_PORT: u16 = 3340;
/// Default QUIC address discovery port. Phase clients look for it exactly here.
pub const DEFAULT_QUIC_PORT: u16 = iroh_relay::defaults::DEFAULT_RELAY_QUIC_PORT;
/// How often to check whether the device list file has changed.
pub const LIST_RELOAD_INTERVAL: Duration = Duration::from_secs(2);

/// Relay server settings.
#[derive(Debug, Clone)]
pub struct RelayOptions {
    /// HTTP address. Without TLS the whole relay runs here; with TLS only the service
    /// "is there internet" check does (usually port 80).
    pub http_addr: SocketAddr,
    /// TLS encryption (for use over the internet). `None` means HTTP only.
    pub tls: Option<TlsOptions>,
    pub access: AccessRules,
    /// Receive rate limit per connection, bytes/s.
    pub client_rate_limit: Option<NonZeroU32>,
    /// Metrics page address (Prometheus format). Do not expose it to the internet.
    pub metrics_addr: Option<SocketAddr>,
}

impl RelayOptions {
    /// A non-TLS relay on the given address, open to everyone (for tests).
    pub fn plain_http(http_addr: SocketAddr) -> Self {
        Self {
            http_addr,
            tls: None,
            access: AccessRules::default(),
            client_rate_limit: None,
            metrics_addr: None,
        }
    }
}

/// HTTPS settings.
#[derive(Debug, Clone)]
pub struct TlsOptions {
    /// HTTPS address (usually port 443).
    pub https_addr: SocketAddr,
    pub cert: CertSource,
    /// QUIC address discovery address (UDP, usually port 7842). `None` means off.
    pub quic_addr: Option<SocketAddr>,
}

/// A running relay. The server stops when dropped.
pub struct RelayServer {
    server: Server,
    access: Arc<RelayAccess>,
    list_watch: Option<AbortOnDrop>,
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl std::fmt::Debug for RelayServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayServer")
            .field("http_addr", &self.server.http_addr())
            .field("https_addr", &self.server.https_addr())
            .field("quic_addr", &self.server.quic_addr())
            .finish_non_exhaustive()
    }
}

impl RelayServer {
    /// Starts the relay.
    pub async fn spawn(opts: RelayOptions) -> Result<Self> {
        let access = Arc::new(RelayAccess::new(&opts.access)?);

        let mut relay = RelayConfig::new(opts.http_addr);
        relay.access = access.clone();
        if let Some(rate) = opts.client_rate_limit {
            let mut limit = ClientRateLimit::new(rate);
            // Short bursts (chunk downloads) are fine: the margin is one second of traffic.
            limit.max_burst_bytes = Some(rate);
            relay.limits.client_rx = Some(limit);
        }
        let mut quic = None;
        if let Some(tls) = &opts.tls {
            let cert = certs::cert_config(&tls.cert).await?;
            relay.tls = Some(TlsConfig::new(tls.https_addr, cert));
            quic = tls.quic_addr.map(QuicConfig::new);
        }

        let mut config = ServerConfig::default();
        config.relay = Some(relay);
        config.quic = quic;
        config.metrics_addr = opts.metrics_addr;
        let server = Server::spawn(config)
            .await
            .map_err(|err| anyhow!("{err:#}"))
            .context("не удалось запустить relay (адрес занят или нет прав на порт?)")?;

        let list_watch = match (access.has_list(), server.relay_service()) {
            (true, Some(service)) => {
                let (access, service) = (access.clone(), service.clone());
                Some(AbortOnDrop(tokio::spawn(async move {
                    let mut tick = tokio::time::interval(LIST_RELOAD_INTERVAL);
                    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        tick.tick().await;
                        for (conn, id) in access.reload_list() {
                            info!(
                                "relay: устройство {} удалено из списка — отключаю",
                                id.fmt_short()
                            );
                            service.clients().disconnect(id, Some(conn));
                        }
                    }
                })))
            }
            _ => None,
        };

        if access.is_open() {
            warn!(
                "relay открыт для всех: любой, кто узнает адрес, сможет пересылать через него \
                 свой трафик. Для работы через интернет задайте пароль (--token-file) или \
                 список устройств (--allow-file)"
            );
        }
        Ok(Self {
            server,
            access,
            list_watch,
        })
    }

    /// HTTP address (without TLS, the address of the relay itself).
    pub fn http_addr(&self) -> Option<SocketAddr> {
        self.server.http_addr()
    }

    /// HTTPS address, if TLS is on.
    pub fn https_addr(&self) -> Option<SocketAddr> {
        self.server.https_addr()
    }

    /// QUIC address discovery address, if on.
    pub fn quic_addr(&self) -> Option<SocketAddr> {
        self.server.quic_addr()
    }

    /// Relay address for clients on the same computer: `http://127.0.0.1:<port>` or
    /// `https://<name>:<port>` (the name must be in the certificate).
    pub fn local_url(&self, tls_name: &str) -> RelayUrl {
        let text = match self.https_addr() {
            Some(addr) => format!("https://{tls_name}:{}", addr.port()),
            None => {
                let addr = self.http_addr().expect("a non-TLS relay listens on HTTP");
                format!("http://127.0.0.1:{}", addr.port())
            }
        };
        text.parse().expect("valid relay address")
    }

    /// How many packets the relay has forwarded between devices since start.
    pub fn relayed_packets(&self) -> u64 {
        self.server.metrics().server.send_packets_sent.get()
    }

    /// How many bytes the relay has received from devices since start.
    pub fn received_bytes(&self) -> u64 {
        self.server.metrics().server.bytes_recv.get()
    }

    /// Whether a device is connected to the relay right now.
    pub fn is_connected(&self, id: &EndpointId) -> bool {
        self.access.is_connected(id)
    }

    /// How many relay connections are open right now.
    pub fn connections(&self) -> usize {
        self.access.connections()
    }

    /// Graceful shutdown: waits for all connections to close.
    pub async fn shutdown(mut self) -> Result<()> {
        self.list_watch.take();
        self.server
            .shutdown()
            .await
            .map_err(|err| anyhow!("relay остановился с ошибкой: {err:#}"))
    }
}
