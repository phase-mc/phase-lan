//! Modrinth is the first stage of mod download: if a mod is published there, the guest downloads
//! it from Modrinth's servers and does not occupy the host's channel.
//!
//! A mod is looked up by the SHA-1 of its file: `GET /v2/version_file/<sha1>?algorithm=sha1`. The reply is
//! a project version with a list of files; the one whose SHA-1 matched is needed. The file is downloaded
//! only from allowed addresses (the Modrinth CDN) and after the download is checked by BLAKE3 against
//! the host's mod: whatever the server answers, only that very same file lands in the mods folder.
//!
//! Only the mod's hash goes out in the requests. They contain no names, no device IDs and no tickets.

use std::{path::Path, time::Duration};

use anyhow::{Context, Result, bail};
use phase_lang::tr;
use phase_minecraft::mods as mc_mods;
use phase_tunnel::mods::ModEntry;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::options::ModrinthSource;

// Phase Client: profile (pack) requests — latest version, search, SHA-512 downloads.
#[cfg(feature = "client")]
mod packs;
#[cfg(feature = "client")]
pub use packs::SearchHit;
#[cfg(feature = "client")]
pub(crate) use packs::{Latest, sha512_file};

/// How long to wait for a connection to the server.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait for the next chunk of the file.
const READ_TIMEOUT: Duration = Duration::from_secs(20);
/// Maximum size of an API reply (a version description is a few kilobytes).
const MAX_API_REPLY: usize = 1024 * 1024;

/// The answer to "is this mod on Modrinth".
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// Yes: the file address.
    Found(String),
    /// There is no such file on Modrinth.
    NotFound,
    /// Modrinth does not answer or answers with an error: the reason.
    Unavailable(String),
}

pub(crate) struct Client {
    http: reqwest::Client,
    /// For profile files: redirects are forbidden, otherwise an allowed address could
    /// steer the download to someone else's server.
    #[cfg(feature = "client")]
    files: reqwest::Client,
    source: ModrinthSource,
}

impl Client {
    pub fn new(source: ModrinthSource) -> Result<Self> {
        // reqwest is built without its own cipher set: we use ring, as iroh does. An error
        // means the set is already installed, which is not a problem.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let builder = || {
            reqwest::Client::builder()
                .user_agent(concat!("Phase/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(CONNECT_TIMEOUT)
                .read_timeout(READ_TIMEOUT)
        };
        let http = builder().build().context(tr!(
            "не удалось подготовить подключение к Modrinth",
            "could not set up the connection to Modrinth"
        ))?;
        #[cfg(feature = "client")]
        let files = builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context(tr!(
                "не удалось подготовить подключение к Modrinth",
                "could not set up the connection to Modrinth"
            ))?;
        Ok(Self {
            http,
            #[cfg(feature = "client")]
            files,
            source,
        })
    }

    /// A GET request to the API, the reply is JSON. `None` means Modrinth answered 404. The whole request
    /// is limited by `timeout`.
    async fn get_json(&self, url: &str, timeout: Duration) -> Result<Option<Value>, String> {
        let request = async {
            let mut response = self.http.get(url).send().await?;
            let status = response.status();
            let mut body = Vec::new();
            if status.is_success() {
                while let Some(chunk) = response.chunk().await? {
                    body.extend_from_slice(&chunk);
                    if body.len() > MAX_API_REPLY {
                        break;
                    }
                }
            }
            Ok::<_, reqwest::Error>((status, body))
        };
        let (status, body) = match tokio::time::timeout(timeout, request).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(err)) if err.is_connect() => {
                return Err(tr!("нет связи", "no connection").into());
            }
            Ok(Err(_)) => return Err(tr!("сбой запроса", "request failed").into()),
            Err(_) => {
                return Err(tr!(
                    format!("нет ответа за {} с", timeout.as_secs()),
                    format!("no answer within {} s", timeout.as_secs())
                ));
            }
        };
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(tr!(
                format!("ответ {}", status.as_u16()),
                format!("response {}", status.as_u16())
            ));
        }
        serde_json::from_slice(&body)
            .map(Some)
            .map_err(|_| tr!("непонятный ответ", "unreadable response").to_string())
    }

    /// Looks up a mod by SHA-1. The whole request is limited by
    /// [`ModrinthSource::lookup_timeout`].
    pub async fn lookup(&self, entry: &ModEntry) -> Lookup {
        let url = format!(
            "{}/v2/version_file/{}?algorithm=sha1",
            self.source.api.trim_end_matches('/'),
            entry.sha1
        );
        match self.get_json(&url, self.source.lookup_timeout).await {
            Ok(Some(version)) => match self.file_url(&version, entry) {
                Some(url) => Lookup::Found(url),
                None => Lookup::NotFound,
            },
            Ok(None) => Lookup::NotFound,
            Err(why) => Lookup::Unavailable(why),
        }
    }

    fn url_allowed(&self, url: &str) -> bool {
        self.source
            .cdn_prefixes
            .iter()
            .any(|prefix| url.starts_with(prefix.as_str()))
    }

    /// The address of the needed file in the version description: the SHA-1 matched, the size is the same, the address is from
    /// the allowed ones.
    fn file_url(&self, version: &Value, entry: &ModEntry) -> Option<String> {
        version.get("files")?.as_array()?.iter().find_map(|file| {
            let sha1 = file.get("hashes")?.get("sha1")?.as_str()?;
            if !sha1.eq_ignore_ascii_case(&entry.sha1) {
                return None;
            }
            if file
                .get("size")
                .and_then(Value::as_u64)
                .is_some_and(|size| size != entry.size)
            {
                return None;
            }
            let url = file.get("url")?.as_str()?;
            self.url_allowed(url).then(|| url.to_string())
        })
    }

    /// Downloads a file into `part` and checks its BLAKE3 against the host's mod. `progress` receives
    /// the number of downloaded bytes. On an error the calling code deletes `part`.
    pub async fn download(
        &self,
        url: &str,
        entry: &ModEntry,
        part: &Path,
        mut progress: impl FnMut(u64) + Send,
    ) -> Result<()> {
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|err| {
                anyhow::anyhow!(
                    "{}: {}",
                    tr!("Modrinth не отдал файл", "Modrinth did not return the file"),
                    err.without_url()
                )
            })?;
        if response
            .content_length()
            .is_some_and(|len| len != entry.size)
        {
            bail!(tr!(
                "файл на Modrinth другого размера",
                "the file on Modrinth has a different size"
            ));
        }
        let mut file = tokio::fs::File::create(part).await.with_context(|| {
            tr!(
                format!("не удалось создать {}", part.display()),
                format!("could not create {}", part.display())
            )
        })?;
        let mut have = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|err| {
            anyhow::anyhow!(
                "{}: {}",
                tr!(
                    "загрузка с Modrinth оборвалась",
                    "the download from Modrinth was interrupted"
                ),
                err.without_url()
            )
        })? {
            have += chunk.len() as u64;
            if have > entry.size {
                bail!(tr!(
                    "файл на Modrinth больше, чем мод хоста",
                    "the file on Modrinth is larger than the host's mod"
                ));
            }
            file.write_all(&chunk).await.with_context(|| {
                tr!(
                    format!("не удалось записать {}", part.display()),
                    format!("could not write {}", part.display())
                )
            })?;
            progress(have);
        }
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        if have != entry.size {
            bail!(tr!(
                "файл с Modrinth скачан не полностью",
                "the file from Modrinth was not fully downloaded"
            ));
        }
        let path = part.to_path_buf();
        let (_, hashes) = tokio::task::spawn_blocking(move || mc_mods::hash_file(&path))
            .await
            .context(tr!(
                "проверка скачанного мода аварийно завершилась",
                "verifying the downloaded mod crashed"
            ))??;
        if hashes.blake3 != entry.blake3 {
            bail!(tr!(
                "файл с Modrinth отличается от мода хоста",
                "the file from Modrinth differs from the host's mod"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> ModEntry {
        ModEntry {
            file_name: "sodium.jar".into(),
            size: 100,
            sha1: "a".repeat(40),
            blake3: "b".repeat(64),
        }
    }

    #[test]
    fn picks_the_file_with_the_same_sha1_from_an_allowed_address() {
        let client = Client::new(ModrinthSource::default()).unwrap();
        // This is how Modrinth answers: a version may have several files.
        let version = serde_json::json!({
            "project_id": "AANobbMI",
            "files": [
                {
                    "hashes": { "sha1": "c".repeat(40), "sha512": "x" },
                    "url": "https://cdn.modrinth.com/data/AANobbMI/versions/1/sources.jar",
                    "filename": "sources.jar", "primary": false, "size": 50
                },
                {
                    "hashes": { "sha1": "A".repeat(40), "sha512": "y" },
                    "url": "https://cdn.modrinth.com/data/AANobbMI/versions/1/sodium.jar",
                    "filename": "sodium.jar", "primary": true, "size": 100
                }
            ]
        });
        assert_eq!(
            client.file_url(&version, &entry()).as_deref(),
            Some("https://cdn.modrinth.com/data/AANobbMI/versions/1/sodium.jar")
        );

        let with = |change: &dyn Fn(&mut Value)| {
            let mut version = version.clone();
            change(&mut version["files"][1]);
            client.file_url(&version, &entry())
        };
        // Someone else's address, a different size or a different hash: the file is "not there".
        assert_eq!(
            with(&|f| f["url"] = "https://evil.example/sodium.jar".into()),
            None
        );
        assert_eq!(
            with(&|f| f["url"] = "http://cdn.modrinth.com/data/sodium.jar".into()),
            None
        );
        assert_eq!(with(&|f| f["size"] = 101.into()), None);
        assert_eq!(with(&|f| f["hashes"]["sha1"] = "d".repeat(40).into()), None);
        assert_eq!(client.file_url(&serde_json::json!({}), &entry()), None);
    }
}
