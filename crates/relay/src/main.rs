//! `phase-relay`: Phase's own relay server.
//!
//! Examples:
//! - testing on a home network (without TLS): `phase-relay`
//!   → for clients: `--relay http://<this PC's IP>:3340`;
//! - VPS with a domain: `phase-relay --acme-domain relay.example.com --token-file tokens.txt`
//!   → for clients: `--relay https://relay.example.com --relay-token <password>`;
//! - VPS without a domain: `phase-relay --self-signed 203.0.113.5 --token-file tokens.txt`
//!   → for clients: `--relay https://203.0.113.5 --relay-ca relay-cert.pem --relay-token <password>`.

use std::{
    net::{Ipv4Addr, SocketAddr, UdpSocket},
    num::NonZeroU32,
    path::PathBuf,
};

use anyhow::{Result, bail};
use clap::{ArgGroup, Parser};
use phase_relay::{
    AccessRules, CertSource, CodesOptions, CodesServer, DEFAULT_DEV_HTTP_PORT, DEFAULT_QUIC_PORT,
    RelayOptions, RelayServer, TlsOptions, access,
};
use tracing::{info, warn};

/// Environment variable with the relay password (instead of a file).
const TOKEN_ENV: &str = "PHASE_RELAY_TOKEN";
/// Former variable name (LanBridge): read if the new one is not set.
const LEGACY_TOKEN_ENV: &str = "LANBRIDGE_RELAY_TOKEN";

#[derive(Parser, Debug)]
#[command(
    name = "phase-relay",
    version,
    about = "Phase relay — собственный relay-сервер для игроков Phase",
    // HTTPS options only make sense together with a certificate.
    group(ArgGroup::new("tls").args(["cert", "acme_domain", "self_signed"]).multiple(true))
)]
struct Cli {
    /// HTTP address. Without a certificate the whole relay runs here (default 0.0.0.0:3340);
    /// with a certificate only the connectivity check does (default 0.0.0.0:80).
    #[arg(long, value_name = "ADDR:PORT")]
    http: Option<SocketAddr>,
    /// HTTPS address (certificate only).
    #[arg(
        long,
        value_name = "ADDR:PORT",
        default_value = "0.0.0.0:443",
        requires = "tls"
    )]
    https: SocketAddr,
    /// QUIC address discovery address, UDP (certificate only). Clients look for it on 7842.
    #[arg(long, value_name = "ADDR:PORT", default_value_t = SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_QUIC_PORT)), requires = "tls")]
    quic: SocketAddr,
    /// Do not run QUIC address discovery.
    #[arg(long, requires = "tls")]
    no_quic: bool,

    /// A ready certificate (PEM, full chain), for example from certbot. Needs --key too.
    #[arg(long, value_name = "FILE", requires = "key", conflicts_with_all = ["acme_domain", "self_signed"])]
    cert: Option<PathBuf>,
    /// The certificate's private key (PEM).
    #[arg(long, value_name = "FILE", requires = "cert")]
    key: Option<PathBuf>,
    /// Get a Let's Encrypt certificate for a domain (port 443 must be open to the
    /// internet). Can be given several times.
    #[arg(long, value_name = "DOMAIN", conflicts_with = "self_signed")]
    acme_domain: Vec<String>,
    /// Email for Let's Encrypt notifications (optional).
    #[arg(long, value_name = "EMAIL", requires = "acme_domain")]
    acme_contact: Option<String>,
    /// Folder for Let's Encrypt certificates.
    #[arg(long, value_name = "DIR", default_value = "acme-cache")]
    acme_cache: PathBuf,
    /// Let's Encrypt staging server (to test the setup; browsers and clients do not trust it).
    #[arg(long, requires = "acme_domain")]
    acme_staging: bool,
    /// Create a self-signed certificate for this name (domain or server IP address).
    /// The relay-cert.pem file must be given to players (--relay-ca). Can be given several times.
    #[arg(long, value_name = "NAME")]
    self_signed: Vec<String>,
    /// Folder for the self-signed certificate.
    #[arg(long, value_name = "DIR", default_value = ".")]
    cert_dir: PathBuf,

    /// File of access passwords (one per line). The password can also be set with the
    /// PHASE_RELAY_TOKEN environment variable (the former name LANBRIDGE_RELAY_TOKEN is read too).
    #[arg(long, value_name = "FILE")]
    token_file: Option<PathBuf>,
    /// File with the list of devices allowed to use the relay (one ID per line).
    /// Changes apply on the fly.
    #[arg(long, value_name = "FILE")]
    allow_file: Option<PathBuf>,
    /// Receive rate limit per device, KiB/s (no limit by default).
    #[arg(long, value_name = "KIB_PER_S")]
    client_rate_kib: Option<NonZeroU32>,
    /// Metrics page address (Prometheus), for example 127.0.0.1:9090. Do not expose it to the internet.
    #[arg(long, value_name = "ADDR:PORT")]
    metrics: Option<SocketAddr>,
    /// Enable the room code service (K7F-2QX → invite) on this address, for example
    /// 0.0.0.0:3341. It runs over HTTP; for HTTPS put a reverse proxy in front of it.
    #[arg(long, value_name = "ADDR:PORT")]
    codes: Option<SocketAddr>,
    /// The code service is behind your own reverse proxy: the player IP for limits is taken from
    /// the X-Forwarded-For header. Do not enable without a proxy: the header is easy to forge.
    #[arg(long, requires = "codes")]
    codes_behind_proxy: bool,

    /// Print a new random password and exit.
    #[arg(long, exclusive = true)]
    generate_token: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.generate_token {
        println!("{}", access::generate_token()?);
        return Ok(());
    }
    init_logging();

    let tls = tls_options(&cli);
    let http_addr = cli.http.unwrap_or_else(|| {
        let port = if tls.is_some() {
            80
        } else {
            DEFAULT_DEV_HTTP_PORT
        };
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, port))
    });
    let mut tokens = Vec::new();
    if let Some(path) = &cli.token_file {
        tokens = access::read_tokens(path)?;
    }
    if let Some(token) = [TOKEN_ENV, LEGACY_TOKEN_ENV]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|token| token.trim().to_string())
        .find(|token| !token.is_empty())
    {
        tokens.push(token);
    }
    let client_rate_limit = match cli.client_rate_kib {
        Some(kib) => match kib.get().checked_mul(1024).and_then(NonZeroU32::new) {
            Some(bytes) => Some(bytes),
            None => bail!("--client-rate-kib: слишком большое значение"),
        },
        None => None,
    };
    let opts = RelayOptions {
        http_addr,
        tls,
        access: AccessRules {
            tokens,
            allow_file: cli.allow_file.clone(),
        },
        client_rate_limit,
        metrics_addr: cli.metrics,
    };
    let has_tokens = !opts.access.tokens.is_empty();
    let server = RelayServer::spawn(opts.clone()).await?;
    print_how_to_connect(&cli, &opts, &server, has_tokens);
    let _codes = match cli.codes {
        Some(addr) => {
            let mut codes = CodesOptions::new(addr);
            codes.trust_forwarded = cli.codes_behind_proxy;
            let codes = CodesServer::spawn(codes).await?;
            println!(
                "Сервис кодов комнат: http://<адрес сервера>:{} — впишите его в окне Phase \
                 («Сеть» → «Сервис кодов»).",
                codes.local_addr().port()
            );
            Some(codes)
        }
        None => None,
    };

    tokio::signal::ctrl_c().await?;
    info!("остановка relay…");
    server.shutdown().await
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("phase_relay=info,iroh_relay=warn,warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
}

fn tls_options(cli: &Cli) -> Option<TlsOptions> {
    let cert = if let (Some(cert), Some(key)) = (&cli.cert, &cli.key) {
        CertSource::Files {
            cert: cert.clone(),
            key: key.clone(),
        }
    } else if !cli.acme_domain.is_empty() {
        CertSource::LetsEncrypt {
            domains: cli.acme_domain.clone(),
            contact: cli.acme_contact.clone(),
            cache_dir: cli.acme_cache.clone(),
            staging: cli.acme_staging,
        }
    } else if !cli.self_signed.is_empty() {
        CertSource::SelfSigned {
            names: cli.self_signed.clone(),
            dir: cli.cert_dir.clone(),
        }
    } else {
        return None;
    };
    Some(TlsOptions {
        https_addr: cli.https,
        cert,
        quic_addr: (!cli.no_quic).then_some(cli.quic),
    })
}

/// Hint: which address to give players.
fn print_how_to_connect(cli: &Cli, opts: &RelayOptions, server: &RelayServer, has_tokens: bool) {
    let token_hint = if has_tokens {
        " --relay-token <пароль>"
    } else {
        ""
    };
    match &opts.tls {
        None => {
            let port = server.http_addr().map(|a| a.port()).unwrap_or_default();
            let host = lan_ip().map_or("<IP этого ПК>".to_string(), |ip| ip.to_string());
            warn!(
                "relay работает БЕЗ TLS (режим для домашней сети и проверок). Игровой трафик \
                 всё равно зашифрован, но пароль relay виден в сети. Для интернета запустите \
                 relay с сертификатом (--acme-domain, --cert или --self-signed)"
            );
            println!("Relay запущен (HTTP, порт {port}).");
            println!("Игрокам в этой сети: --relay http://{host}:{port}{token_hint}");
        }
        Some(tls) => {
            let port = server.https_addr().map(|a| a.port()).unwrap_or(443);
            let port_part = if port == 443 {
                String::new()
            } else {
                format!(":{port}")
            };
            let (name, ca_hint) = match &tls.cert {
                CertSource::LetsEncrypt { domains, .. } => (domains[0].clone(), String::new()),
                CertSource::SelfSigned { names, dir } => (
                    names[0].clone(),
                    format!(
                        " --relay-ca {}",
                        dir.join(phase_relay::certs::SELF_SIGNED_CERT_FILE)
                            .display()
                    ),
                ),
                CertSource::Files { .. } => ("<домен из сертификата>".to_string(), String::new()),
            };
            println!("Relay запущен (HTTPS, порт {port}).");
            if let Some(quic) = server.quic_addr() {
                println!(
                    "QUIC address discovery: UDP-порт {} (откройте его в брандмауэре сервера).",
                    quic.port()
                );
                if quic.port() != DEFAULT_QUIC_PORT {
                    warn!(
                        "клиенты ищут QUIC address discovery на порту {DEFAULT_QUIC_PORT}: \
                         на другом порту они его не найдут (relay при этом работает)"
                    );
                }
            }
            println!("Игрокам: --relay https://{name}{port_part}{ca_hint}{token_hint}");
            if matches!(tls.cert, CertSource::LetsEncrypt { .. }) {
                println!(
                    "Сертификат Let's Encrypt будет получен при первом подключении (папка {}).",
                    cli.acme_cache.display()
                );
            }
        }
    }
    if opts.access.is_open() {
        println!("Доступ: открыт для всех.");
    } else {
        let mut parts = Vec::new();
        if has_tokens {
            parts.push(format!("по паролю ({} шт.)", opts.access.tokens.len()));
        }
        if let Some(file) = &opts.access.allow_file {
            parts.push(format!("по списку устройств {}", file.display()));
        }
        println!("Доступ: {}.", parts.join(" или "));
    }
    println!("Остановить: Ctrl+C.");
}

/// IP address of this computer on the LAN (for the hint). The UDP socket "connects"
/// to an external address only so the OS picks a network interface: no packets are sent.
fn lan_ip() -> Option<std::net::IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn plain_http_by_default_and_tls_with_certificate() {
        let cli = Cli::parse_from(["phase-relay"]);
        assert!(tls_options(&cli).is_none());

        let cli = Cli::parse_from(["phase-relay", "--self-signed", "203.0.113.5"]);
        let tls = tls_options(&cli).unwrap();
        assert!(matches!(tls.cert, CertSource::SelfSigned { .. }));
        assert_eq!(tls.quic_addr.unwrap().port(), DEFAULT_QUIC_PORT);

        let cli = Cli::parse_from([
            "phase-relay",
            "--acme-domain",
            "relay.example.com",
            "--no-quic",
        ]);
        let tls = tls_options(&cli).unwrap();
        assert!(matches!(
            tls.cert,
            CertSource::LetsEncrypt { staging: false, .. }
        ));
        assert!(tls.quic_addr.is_none());
    }

    #[test]
    fn conflicting_certificate_sources_are_rejected() {
        assert!(
            Cli::try_parse_from([
                "phase-relay",
                "--acme-domain",
                "a.example",
                "--self-signed",
                "1.2.3.4"
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["phase-relay", "--cert", "c.pem"]).is_err());
        assert!(Cli::try_parse_from(["phase-relay", "--acme-staging"]).is_err());
        // HTTPS options without a certificate are an error, not silently ignored.
        assert!(Cli::try_parse_from(["phase-relay", "--https", "0.0.0.0:8443"]).is_err());
        assert!(Cli::try_parse_from(["phase-relay", "--no-quic"]).is_err());
        assert!(
            Cli::try_parse_from([
                "phase-relay",
                "--self-signed",
                "1.2.3.4",
                "--https",
                "0.0.0.0:8443"
            ])
            .is_ok()
        );
    }
}
