//! Who may use the relay.
//!
//! The relay cannot see traffic contents (encryption is device to device), but anyone
//! can use an open relay: push their own traffic through it at our
//! expense. So access can be restricted:
//! - by **password** (`token`): the client sends it when connecting to the relay;
//! - by **device list** (a file of IDs, one per line, like the host's `allowed.txt`).
//!
//! If both are set, either one is enough. If neither is set, the relay
//! is open to everyone (for testing only).
//!
//! The list file is reread on the fly: added devices are let in right away,
//! and removed ones are disconnected (if they came in by the list, not by password).

use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, RwLock},
    time::SystemTime,
};

use anyhow::{Context, Result, bail};
use iroh_base::EndpointId;
use iroh_relay::server::{Access, AccessControl, ClientRequest, ConnectionId};
use subtle::ConstantTimeEq;
use tracing::{debug, info, warn};

/// Minimum relay password length: a short password is easy to guess.
pub const MIN_TOKEN_LEN: usize = 16;

/// Rejection reason the client will see.
pub const DENY_REASON: &str = "relay is private: wrong or missing token, device not in allow list";

/// Access rules.
#[derive(Debug, Default, Clone)]
pub struct AccessRules {
    /// Access passwords (any of them works). Empty means no password check.
    pub tokens: Vec<String>,
    /// File with the list of allowed devices.
    pub allow_file: Option<PathBuf>,
}

impl AccessRules {
    /// Relay open to everyone.
    pub fn is_open(&self) -> bool {
        self.tokens.is_empty() && self.allow_file.is_none()
    }
}

/// Reads passwords from a file: one per line, empty lines and lines with `#` are skipped.
pub fn read_tokens(path: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("не удалось прочитать файл паролей {}", path.display()))?;
    let tokens: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect();
    if tokens.is_empty() {
        bail!("в файле паролей {} нет ни одного пароля", path.display());
    }
    Ok(tokens)
}

/// Checks a password: at least [`MIN_TOKEN_LEN`] long and without spaces (the password goes in
/// an HTTP header, where spaces and control characters are not allowed).
pub fn check_token(token: &str) -> Result<()> {
    if token.chars().count() < MIN_TOKEN_LEN {
        bail!(
            "пароль relay слишком короткий (меньше {MIN_TOKEN_LEN} символов). \
             Создайте надёжный: phase-relay --generate-token"
        );
    }
    if !token.chars().all(|c| c.is_ascii_graphic()) {
        bail!("пароль relay может содержать только латинские буквы, цифры и знаки, без пробелов");
    }
    Ok(())
}

/// New random password: 24 bytes from the OS generator → 48 hex characters.
pub fn generate_token() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn getrandom(buf: &mut [u8]) -> Result<()> {
    // The iroh device key is also created from the OS generator; we take the same one
    // through rustls (ring) to avoid an extra dependency.
    use rustls::crypto::{CryptoProvider, ring};
    let provider: CryptoProvider = ring::default_provider();
    provider
        .secure_random
        .fill(buf)
        .map_err(|_| anyhow::anyhow!("генератор случайных чисел ОС недоступен"))
}

/// How a connection got access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Via {
    Open,
    Token,
    List,
}

/// File modification time and size: they show that the file has changed.
type Stamp = (SystemTime, u64);

/// State of the list file: what was read and when the file changed.
#[derive(Debug)]
struct ListFile {
    path: PathBuf,
    ids: RwLock<BTreeSet<EndpointId>>,
    /// File modification time and size at the last read.
    stamp: Mutex<Option<Stamp>>,
}

/// Relay access check (plugs into the iroh-relay server).
#[derive(Debug)]
pub struct RelayAccess {
    tokens: Vec<Vec<u8>>,
    list: Option<ListFile>,
    /// Current relay connections: how each one got access.
    connected: Mutex<HashMap<ConnectionId, (EndpointId, Via)>>,
}

impl RelayAccess {
    pub fn new(rules: &AccessRules) -> Result<Self> {
        for token in &rules.tokens {
            check_token(token)?;
        }
        let list = match &rules.allow_file {
            Some(path) => {
                let file = ListFile {
                    path: path.clone(),
                    ids: RwLock::default(),
                    stamp: Mutex::default(),
                };
                // The file must be readable at startup: otherwise the relay would silently reject everyone.
                let (ids, stamp) = read_list(path)?;
                info!(
                    "список устройств {}: разрешено {}",
                    path.display(),
                    ids.len()
                );
                *file.ids.write().expect("relay list lock") = ids;
                *file.stamp.lock().expect("relay list lock") = stamp;
                Some(file)
            }
            None => None,
        };
        Ok(Self {
            tokens: rules.tokens.iter().map(|t| t.as_bytes().to_vec()).collect(),
            list,
            connected: Mutex::default(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.tokens.is_empty() && self.list.is_none()
    }

    pub fn has_list(&self) -> bool {
        self.list.is_some()
    }

    /// Admission decision (not counting connections).
    fn decide(&self, id: &EndpointId, token: Option<&str>) -> Option<Via> {
        if self.is_open() {
            return Some(Via::Open);
        }
        if let Some(token) = token
            && self.token_matches(token.as_bytes())
        {
            return Some(Via::Token);
        }
        if let Some(list) = &self.list
            && list.ids.read().expect("relay list lock").contains(id)
        {
            return Some(Via::List);
        }
        None
    }

    /// Constant-time comparison: go through all passwords without an early exit,
    /// so the response time cannot be used to guess the password character by character.
    fn token_matches(&self, given: &[u8]) -> bool {
        let mut found = subtle::Choice::from(0);
        for token in &self.tokens {
            found |= token.as_slice().ct_eq(given);
        }
        found.into()
    }

    /// Whether a device is connected to the relay right now.
    pub fn is_connected(&self, id: &EndpointId) -> bool {
        self.connected
            .lock()
            .expect("relay connections lock")
            .values()
            .any(|(connected, _)| connected == id)
    }

    /// How many connections are open right now.
    pub fn connections(&self) -> usize {
        self.connected.lock().expect("relay connections lock").len()
    }

    /// Rereads the list file if it changed. Returns the connections that
    /// came in by the list but are no longer on it: they must be disconnected.
    ///
    /// If the file cannot be read or has an error, the previous list stays: the relay does not
    /// become open to everyone or closed to everyone because of a half-written file.
    pub fn reload_list(&self) -> Vec<(ConnectionId, EndpointId)> {
        let Some(list) = &self.list else {
            return Vec::new();
        };
        let current = file_stamp(&list.path);
        {
            let mut stamp = list.stamp.lock().expect("relay list lock");
            if current.is_some() && current == *stamp {
                return Vec::new();
            }
            match read_list(&list.path) {
                Ok((ids, new_stamp)) => {
                    info!(
                        "список устройств {} перечитан: разрешено {}",
                        list.path.display(),
                        ids.len()
                    );
                    *list.ids.write().expect("relay list lock") = ids;
                    *stamp = new_stamp;
                }
                Err(err) => {
                    // Remember the "broken" state so we do not log a warning
                    // every few seconds; a fixed file will be read.
                    if current != *stamp {
                        warn!("{err:#}. Пока действует прежний список устройств");
                    }
                    *stamp = current;
                    return Vec::new();
                }
            }
        }
        let ids = list.ids.read().expect("relay list lock");
        self.connected
            .lock()
            .expect("relay connections lock")
            .iter()
            .filter(|(_, (id, via))| *via == Via::List && !ids.contains(id))
            .map(|(conn, (id, _))| (*conn, *id))
            .collect()
    }
}

impl AccessControl for RelayAccess {
    async fn on_connect(&self, request: &ClientRequest) -> Access {
        let id = request.endpoint_id();
        let token = request.auth_token();
        match self.decide(&id, token.as_deref()) {
            Some(via) => {
                debug!(
                    "relay: устройство {} подключилось ({via:?})",
                    id.fmt_short()
                );
                self.connected
                    .lock()
                    .expect("relay connections lock")
                    .insert(request.connection_id(), (id, via));
                Access::Allow
            }
            None => {
                info!(
                    "relay: устройству {} отказано ({})",
                    id.fmt_short(),
                    if token.is_some() {
                        "неверный пароль, нет в списке"
                    } else {
                        "нет пароля, нет в списке"
                    }
                );
                Access::Deny {
                    reason: Some(DENY_REASON.to_string()),
                }
            }
        }
    }

    fn on_disconnect(&self, endpoint_id: EndpointId, connection_id: ConnectionId) {
        debug!("relay: устройство {} отключилось", endpoint_id.fmt_short());
        self.connected
            .lock()
            .expect("relay connections lock")
            .remove(&connection_id);
    }
}

fn file_stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Reads the device list: one ID per line, `#` starts a comment (also after an ID).
fn read_list(path: &Path) -> Result<(BTreeSet<EndpointId>, Option<Stamp>)> {
    let stamp = file_stamp(path);
    let text = fs::read_to_string(path)
        .with_context(|| format!("не удалось прочитать список устройств {}", path.display()))?;
    Ok((parse_list(&text, path)?, stamp))
}

fn parse_list(text: &str, path: &Path) -> Result<BTreeSet<EndpointId>> {
    let mut ids = BTreeSet::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let id: EndpointId = line.parse().with_context(|| {
            format!(
                "в списке устройств {} (строка {}) неверный ID: {line}",
                path.display(),
                n + 1
            )
        })?;
        ids.insert(id);
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use iroh_base::SecretKey;

    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phase-relay-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("allowed.txt")
    }

    fn id() -> EndpointId {
        SecretKey::generate().public()
    }

    const TOKEN: &str = "0123456789abcdef0123";

    #[test]
    fn open_relay_admits_everyone() {
        let access = RelayAccess::new(&AccessRules::default()).unwrap();
        assert!(access.is_open());
        assert_eq!(access.decide(&id(), None), Some(Via::Open));
    }

    #[test]
    fn token_is_checked_exactly() {
        let rules = AccessRules {
            tokens: vec![TOKEN.into(), "another-long-token-xyz".into()],
            allow_file: None,
        };
        let access = RelayAccess::new(&rules).unwrap();
        assert_eq!(access.decide(&id(), Some(TOKEN)), Some(Via::Token));
        assert_eq!(
            access.decide(&id(), Some("another-long-token-xyz")),
            Some(Via::Token)
        );
        assert_eq!(access.decide(&id(), None), None);
        assert_eq!(access.decide(&id(), Some("0123456789abcdef012")), None);
        assert_eq!(access.decide(&id(), Some("0123456789abcdef01234")), None);
        assert_eq!(access.decide(&id(), Some("")), None);
    }

    #[test]
    fn weak_tokens_are_rejected() {
        assert!(check_token("short").is_err());
        assert!(check_token("with spaces inside it!").is_err());
        assert!(check_token("пароль-кириллицей-длинный").is_err());
        assert!(check_token(TOKEN).is_ok());
        let generated = generate_token().unwrap();
        assert_eq!(generated.len(), 48);
        assert!(check_token(&generated).is_ok());
        assert_ne!(generated, generate_token().unwrap());
    }

    #[test]
    fn list_or_token_is_enough() {
        let (allowed, other) = (id(), id());
        let file = temp_file("either");
        fs::write(&file, format!("# друзья\n{allowed}  # Петя\n\n")).unwrap();
        let access = RelayAccess::new(&AccessRules {
            tokens: vec![TOKEN.into()],
            allow_file: Some(file),
        })
        .unwrap();
        assert_eq!(access.decide(&allowed, None), Some(Via::List));
        assert_eq!(access.decide(&other, Some(TOKEN)), Some(Via::Token));
        assert_eq!(access.decide(&other, Some("wrong-token-1234567")), None);
        assert_eq!(access.decide(&other, None), None);
    }

    #[test]
    fn missing_or_broken_list_fails_at_start() {
        let file = temp_file("missing");
        let rules = AccessRules {
            tokens: Vec::new(),
            allow_file: Some(file.clone()),
        };
        assert!(RelayAccess::new(&rules).is_err());
        fs::write(&file, "not-an-id\n").unwrap();
        let err = RelayAccess::new(&rules).unwrap_err();
        assert!(format!("{err:#}").contains("строка 1"), "{err:#}");
    }

    #[test]
    fn list_reload_admits_new_and_revokes_removed() {
        let (a, b) = (id(), id());
        let file = temp_file("reload");
        fs::write(&file, format!("{a}\n")).unwrap();
        let access = RelayAccess::new(&AccessRules {
            tokens: Vec::new(),
            allow_file: Some(file.clone()),
        })
        .unwrap();
        assert_eq!(access.decide(&b, None), None);
        // "Connections" of a: one by the list.
        let conn_a = fake_connection();
        access
            .connected
            .lock()
            .unwrap()
            .insert(conn_a, (a, Via::List));

        // Nothing happens without a file change.
        assert!(access.reload_list().is_empty());

        // Added b, removed a. Modification time is coarse on some file systems, so the size changes too.
        fs::write(&file, format!("# новый список\n{b}\n")).unwrap();
        let revoked = access.reload_list();
        assert_eq!(revoked, vec![(conn_a, a)]);
        assert_eq!(access.decide(&b, None), Some(Via::List));
        assert_eq!(access.decide(&a, None), None);

        // Broken file: the previous list stays.
        fs::write(&file, format!("{b}\nмусор\n")).unwrap();
        assert!(access.reload_list().is_empty());
        assert_eq!(access.decide(&b, None), Some(Via::List));

        // File deleted: also the previous list (not "open to everyone", not "closed to everyone").
        fs::remove_file(&file).unwrap();
        assert!(access.reload_list().is_empty());
        assert_eq!(access.decide(&b, None), Some(Via::List));
    }

    #[test]
    fn token_connections_are_not_revoked_by_list() {
        let (a, b) = (id(), id());
        let file = temp_file("token-stays");
        fs::write(&file, format!("{a}\n")).unwrap();
        let access = RelayAccess::new(&AccessRules {
            tokens: vec![TOKEN.into()],
            allow_file: Some(file.clone()),
        })
        .unwrap();
        let conn = fake_connection();
        access
            .connected
            .lock()
            .unwrap()
            .insert(conn, (b, Via::Token));
        fs::write(
            &file,
            "# пусто, но длиннее прежнего файла ...........................\n",
        )
        .unwrap();
        assert!(access.reload_list().is_empty());
        assert!(access.is_connected(&b));
    }

    /// Unique connection number: only the server issues real ones, so we take one
    /// from an empty "guard" (this is the public way to get a new number).
    fn fake_connection() -> ConnectionId {
        iroh_relay::server::OnDisconnectGuard::empty(id()).connection_id()
    }
}
