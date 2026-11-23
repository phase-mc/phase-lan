//! TLS certificates for the relay.
//!
//! Clients connect to the relay over HTTPS and check the certificate. Options:
//! - **Let's Encrypt**: a free certificate for a domain, the program obtains and
//!   renews it itself (needs a domain and port 443 open to the internet);
//! - **files**: a ready certificate (for example, from certbot); the files are reread
//!   periodically, so renewal does not need a restart;
//! - **self-signed**: for a relay without a domain (for example, by a VPS IP address). Clients
//!   must be given the certificate file (`--relay-ca`), otherwise they refuse to connect.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use iroh_relay::server::{AcmeConfig, CertConfig, reloading_resolver};
use rustls::{ConfigBuilder, ServerConfig, server::WantsServerCert};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use tracing::info;

/// How often to reread certificate files (certbot renews 30 days before expiry).
pub const CERT_RELOAD_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// File names of the self-signed certificate.
pub const SELF_SIGNED_CERT_FILE: &str = "relay-cert.pem";
pub const SELF_SIGNED_KEY_FILE: &str = "relay-key.pem";

/// Where to get the certificate.
#[derive(Debug, Clone)]
pub enum CertSource {
    /// Ready PEM files: the certificate chain and the private key.
    Files { cert: PathBuf, key: PathBuf },
    /// Let's Encrypt (validation through port 443, TLS-ALPN-01).
    LetsEncrypt {
        domains: Vec<String>,
        /// Email for Let's Encrypt notifications (optional).
        contact: Option<String>,
        /// Folder where the obtained certificate is stored. Without it the certificate would be
        /// requested on every start, and Let's Encrypt limits the number of issuances.
        cache_dir: PathBuf,
        /// Let's Encrypt staging server (the certificate is not trusted, but the limits are softer).
        staging: bool,
    },
    /// Self-signed certificate for the given names (domains or IP addresses).
    /// Created once and stored in `dir`.
    SelfSigned { names: Vec<String>, dir: PathBuf },
}

/// Base TLS server config: ring crypto, safe TLS versions.
fn server_config_builder() -> ConfigBuilder<ServerConfig, WantsServerCert> {
    ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports TLS 1.2 and 1.3")
        .with_no_client_auth()
}

/// Prepares the certificate for the iroh-relay server.
pub(crate) async fn cert_config(source: &CertSource) -> Result<CertConfig> {
    let builder = server_config_builder();
    Ok(match source {
        CertSource::Files { cert, key } => {
            let provider = builder.crypto_provider().clone();
            let resolver =
                reloading_resolver(&provider, cert.clone(), key.clone(), CERT_RELOAD_INTERVAL)
                    .await
                    .map_err(|err| {
                        anyhow!(
                            "не удалось загрузить сертификат {} и ключ {}: {err:#}",
                            cert.display(),
                            key.display()
                        )
                    })?;
            CertConfig::Manual {
                server_config: builder.with_cert_resolver(resolver),
            }
        }
        CertSource::LetsEncrypt {
            domains,
            contact,
            cache_dir,
            staging,
        } => {
            if domains.is_empty() {
                bail!("для Let's Encrypt нужен домен (--acme-domain)");
            }
            fs::create_dir_all(cache_dir).with_context(|| {
                format!(
                    "не удалось создать папку сертификатов {}",
                    cache_dir.display()
                )
            })?;
            let mut acme = AcmeConfig::letsencrypt(!staging)
                .domains(domains.clone())
                .cache_path(cache_dir.clone());
            if let Some(mail) = contact {
                acme = acme.contact(vec![format!("mailto:{mail}")]);
            }
            CertConfig::LetsEncrypt {
                acme_config: acme,
                server_config_builder: builder,
            }
        }
        CertSource::SelfSigned { names, dir } => {
            let (cert_path, key_path) = ensure_self_signed(dir, names)?;
            let certs = read_certs(&cert_path)?;
            let key = PrivateKeyDer::from_pem_file(&key_path)
                .with_context(|| format!("не удалось прочитать ключ {}", key_path.display()))?;
            CertConfig::Manual {
                server_config: builder
                    .with_single_cert(certs, key)
                    .context("сертификат не подходит к ключу")?,
            }
        }
    })
}

/// Creates a self-signed certificate in `dir` if it is not there yet.
/// Returns the paths to the certificate and the key.
///
/// An existing certificate is not recreated: clients already trust exactly that one
/// (`--relay-ca`). To issue a new one, delete both files.
pub fn ensure_self_signed(dir: &Path, names: &[String]) -> Result<(PathBuf, PathBuf)> {
    let cert_path = dir.join(SELF_SIGNED_CERT_FILE);
    let key_path = dir.join(SELF_SIGNED_KEY_FILE);
    match (cert_path.exists(), key_path.exists()) {
        (true, true) => {
            info!(
                "используется прежний самоподписанный сертификат {} (чтобы создать новый, удалите {} и {})",
                cert_path.display(),
                SELF_SIGNED_CERT_FILE,
                SELF_SIGNED_KEY_FILE
            );
            return Ok((cert_path, key_path));
        }
        (false, false) => {}
        _ => bail!(
            "в папке {} есть только один из файлов {SELF_SIGNED_CERT_FILE} и {SELF_SIGNED_KEY_FILE}. \
             Удалите оставшийся файл — будет создан новый сертификат",
            dir.display()
        ),
    }
    if names.is_empty() {
        bail!("для самоподписанного сертификата нужно имя: домен или IP-адрес сервера");
    }
    let generated = rcgen::generate_simple_self_signed(names.to_vec())
        .context("не удалось создать сертификат")?;
    fs::create_dir_all(dir)
        .with_context(|| format!("не удалось создать папку {}", dir.display()))?;
    write_new(
        &key_path,
        generated.signing_key.serialize_pem().as_bytes(),
        true,
    )?;
    write_new(&cert_path, generated.cert.pem().as_bytes(), false)?;
    info!(
        "создан самоподписанный сертификат {} для {}. Передайте ЭТОТ файл (не ключ!) игрокам: \
         --relay-ca {}",
        cert_path.display(),
        names.join(", "),
        SELF_SIGNED_CERT_FILE
    );
    Ok((cert_path, key_path))
}

/// Creates a new file (without overwriting an existing one). On Linux the private key
/// is readable only by its owner.
fn write_new(path: &Path, data: &[u8], secret: bool) -> Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = secret; // on Windows a file in the user's folder is already closed to other accounts
    let mut file = options
        .open(path)
        .with_context(|| format!("не удалось создать {}", path.display()))?;
    file.write_all(data)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("не удалось записать {}", path.display()))
}

/// Reads certificates from a PEM file.
pub fn read_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .with_context(|| format!("не удалось открыть сертификат {}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("не удалось прочитать сертификат {}", path.display()))?;
    if certs.is_empty() {
        bail!(
            "в файле {} нет сертификатов (нужен формат PEM)",
            path.display()
        );
    }
    Ok(certs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phase-relay-cert-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[tokio::test]
    async fn self_signed_is_created_once_and_reused() {
        let dir = temp_dir("once");
        let names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let (cert, key) = ensure_self_signed(&dir, &names).unwrap();
        let first = fs::read(&cert).unwrap();
        assert!(fs::read_to_string(&key).unwrap().contains("PRIVATE KEY"));
        assert_eq!(read_certs(&cert).unwrap().len(), 1);

        // Second start: the same certificate (clients already trust it).
        ensure_self_signed(&dir, &names).unwrap();
        assert_eq!(fs::read(&cert).unwrap(), first);
        assert!(
            cert_config(&CertSource::SelfSigned {
                names: names.clone(),
                dir: dir.clone()
            })
            .await
            .is_ok()
        );

        // Only the certificate is left without the key: an error, not a silent replacement.
        fs::remove_file(&key).unwrap();
        assert!(ensure_self_signed(&dir, &names).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn certificate_files_are_loaded() {
        let dir = temp_dir("files");
        let (cert, key) = ensure_self_signed(&dir, &["relay.test".to_string()]).unwrap();
        assert!(
            cert_config(&CertSource::Files {
                cert: cert.clone(),
                key
            })
            .await
            .is_ok()
        );
        let missing = cert_config(&CertSource::Files {
            cert,
            key: dir.join("нет-такого.pem"),
        })
        .await;
        assert!(missing.is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_pem_file_is_an_error() {
        let dir = temp_dir("bad");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.pem");
        fs::write(&path, "это не сертификат").unwrap();
        assert!(read_certs(&path).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
