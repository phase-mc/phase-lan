//! Room mods: the host shares its `.jar` files, the guest takes the missing ones.
//!
//! Runs on top of a [`StreamKind::Mods`] stream. The guest opens the stream and sends one
//! request:
//!
//! - **list**: the host answers with a [`ModManifest`]: name, size and two hashes of each mod;
//! - **file** by its BLAKE3 (and, when resuming, from which byte): the host answers with the size and
//!   then sends the contents until the end of the stream. One file per stream.
//!
//! What protects both sides:
//!
//! - the guest names a file **only by hash** from the list. The host does not accept paths or names
//!   from the guest, so only what the host itself put in the share can be served;
//! - the list from the host is checked on receipt ([`ModManifest::validate`]): the file name is
//!   a plain `something.jar` without path separators or `..`, hashes have the right length, sizes and
//!   the number of mods are capped. A bad list is rejected as a whole;
//! - the file is written to a temporary `.part`, BLAKE3 is computed along the way, and only when
//!   the hash matches does the transfer count as successful. On a mismatch the `.part` is deleted.
//!
//! Only a guest the host has already let into the room opens the stream (see
//! [`crate::access`]).

use phase_lang::tr;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    io::SeekFrom,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use iroh::endpoint::Connection;
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt},
};

use crate::wire::{self, ModsOffer, StreamKind};

/// How many mods the list can hold.
pub const MAX_MODS: usize = 1024;
/// Maximum size of one mod.
pub const MAX_MOD_SIZE: u64 = 2 * 1024 * 1024 * 1024;
/// Maximum length of a mod file name, in characters.
pub const NAME_MAX_CHARS: usize = 200;

/// Maximum size of the mod list message.
const MAX_MANIFEST_MSG: usize = 2 * 1024 * 1024;
/// Maximum size of a guest request and of the "here is the file size" answer.
const MAX_REQUEST_MSG: usize = 4 * 1024;
/// Chunk size for file transfer.
const CHUNK: usize = 64 * 1024;
/// How long to wait for the guest's request and the host's answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for the next file chunk.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);

/// Windows device names: a file with such a name cannot be opened.
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Checks a mod file name received over the network. Only a plain name
/// `something.jar` is allowed: without `/`, `\`, `:`, `..`, control characters (including the null
/// byte), characters forbidden in Windows names; not hidden, no leading or trailing space, not
/// a Windows device name; the extension is `.jar` only.
///
/// `phase_minecraft::mods::validate_file_name` repeats the same rules: there the name
/// is checked a second time, right before touching the disk.
pub fn validate_file_name(name: &str) -> Result<()> {
    let length = name.chars().count();
    if length == 0 {
        bail!(tr!("пустое имя файла мода", "empty mod file name"));
    }
    if length > NAME_MAX_CHARS {
        bail!(tr!(
            format!("имя файла мода длиннее {NAME_MAX_CHARS} знаков"),
            format!("mod file name is longer than {NAME_MAX_CHARS} characters")
        ));
    }
    if name.chars().any(char::is_control) {
        bail!(tr!(
            "в имени файла мода есть управляющие символы",
            "mod file name contains control characters"
        ));
    }
    // From here on the name goes into error text: it has no control characters anymore.
    if let Some(bad) = name
        .chars()
        .find(|c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        bail!(tr!(
            format!("в имени файла мода «{name}» недопустимый знак «{bad}»"),
            format!("mod file name \"{name}\" contains the invalid character \"{bad}\"")
        ));
    }
    if name.contains("..") {
        bail!(tr!(
            format!("в имени файла мода «{name}» недопустимо «..»"),
            format!("mod file name \"{name}\" must not contain \"..\"")
        ));
    }
    if name.starts_with('.') || name.starts_with(' ') || name.ends_with(' ') {
        bail!(tr!(
            format!(
                "имя файла мода «{name}» начинается с точки или пробела либо кончается пробелом"
            ),
            format!("mod file name \"{name}\" starts with a dot or space, or ends with a space")
        ));
    }
    let is_jar = name.len() > 4
        && name.is_char_boundary(name.len() - 4)
        && name[name.len() - 4..].eq_ignore_ascii_case(".jar");
    if !is_jar {
        bail!(tr!(
            format!("«{name}» — не файл .jar"),
            format!("\"{name}\" is not a .jar file")
        ));
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    if RESERVED_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(stem))
    {
        bail!(tr!(
            format!("«{name}» — имя устройства Windows, а не файла"),
            format!("\"{name}\" is a Windows device name, not a file name")
        ));
    }
    Ok(())
}

/// The string is exactly `len` lowercase hex digits.
fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A mod in the host's share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModEntry {
    /// File name (`sodium-0.5.8.jar`). Checked by [`validate_file_name`].
    pub file_name: String,
    pub size: u64,
    /// SHA-1 of the contents, 40 characters: used to look the mod up on Modrinth.
    pub sha1: String,
    /// BLAKE3 of the contents, 64 characters: used to compare the guest's folder and check
    /// the downloaded file.
    pub blake3: String,
}

impl ModEntry {
    pub fn validate(&self) -> Result<()> {
        validate_file_name(&self.file_name)?;
        if self.size > MAX_MOD_SIZE {
            let (name, size) = (&self.file_name, self.size);
            bail!(tr!(
                format!("мод «{name}» слишком большой: {size} байт"),
                format!("mod \"{name}\" is too large: {size} bytes")
            ));
        }
        if !is_hex(&self.sha1, 40) || !is_hex(&self.blake3, 64) {
            let name = &self.file_name;
            bail!(tr!(
                format!("у мода «{name}» неверная запись хэша"),
                format!("mod \"{name}\" has an invalid hash")
            ));
        }
        Ok(())
    }
}

/// The room's mod list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModManifest {
    /// Mods the guest needs.
    #[serde(default)]
    pub mods: Vec<ModEntry>,
    /// BLAKE3 of host mods it does **not** share (purely client-side: shaders, replay
    /// recording). The guest does not need them, but if the guest already has such a mod, it is not extra.
    #[serde(default)]
    pub optional: Vec<String>,
}

impl ModManifest {
    pub fn total_size(&self) -> u64 {
        self.mods.iter().map(|m| m.size).sum()
    }

    /// A short summary of the list, for the hello reply.
    pub fn offer(&self) -> ModsOffer {
        ModsOffer {
            files: u32::try_from(self.mods.len()).unwrap_or(u32::MAX),
            total_size: self.total_size(),
        }
    }

    /// Checks the whole list: the number of mods, every name and hash, duplicate names (case-
    /// insensitive: on Windows it is one file) and duplicate contents.
    pub fn validate(&self) -> Result<()> {
        if self.mods.len() > MAX_MODS || self.optional.len() > MAX_MODS {
            bail!(tr!(
                format!("в списке модов больше {MAX_MODS} записей"),
                format!("the mod list has more than {MAX_MODS} entries")
            ));
        }
        let (mut names, mut hashes) = (HashSet::new(), HashSet::new());
        for entry in &self.mods {
            entry.validate()?;
            if !names.insert(entry.file_name.to_lowercase()) {
                let name = &entry.file_name;
                bail!(tr!(
                    format!("мод «{name}» в списке дважды"),
                    format!("mod \"{name}\" is listed twice")
                ));
            }
            if !hashes.insert(entry.blake3.as_str()) {
                let name = &entry.file_name;
                bail!(tr!(
                    format!("мод «{name}» повторяет содержимое другого мода"),
                    format!("mod \"{name}\" duplicates the contents of another mod")
                ));
            }
        }
        if self.optional.iter().any(|hash| !is_hex(hash, 64)) {
            bail!(tr!(
                "в списке модов неверная запись хэша",
                "the mod list contains an invalid hash"
            ));
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ host share

/// A host mod and where it lives on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedMod {
    pub entry: ModEntry,
    pub path: PathBuf,
}

#[derive(Default)]
struct Shared {
    manifest: ModManifest,
    /// BLAKE3 → file on the host's disk.
    files: HashMap<String, SharedMod>,
}

/// The host's mod share. Shared by all guests; it can change while the room is open
/// (turned on, off, a different set): new requests see the new list right away.
#[derive(Clone, Default)]
pub struct ModShare(Arc<RwLock<Arc<Shared>>>);

impl ModShare {
    /// An empty share: no mods are required.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the share. Mods with identical contents go once (the first in
    /// order). The list is checked as strictly as on the guest: whatever the guest would reject anyway
    /// is not shared.
    pub fn set(&self, mods: Vec<SharedMod>, optional: Vec<String>) -> Result<()> {
        let mut files = HashMap::with_capacity(mods.len());
        let mut manifest = ModManifest {
            mods: Vec::with_capacity(mods.len()),
            optional,
        };
        for shared in mods {
            if files.contains_key(&shared.entry.blake3) {
                continue;
            }
            manifest.mods.push(shared.entry.clone());
            files.insert(shared.entry.blake3.clone(), shared);
        }
        manifest.optional.sort_unstable();
        manifest.optional.dedup();
        manifest.optional.retain(|hash| !files.contains_key(hash));
        manifest.validate()?;
        *self.0.write().expect("mod share lock") = Arc::new(Shared { manifest, files });
        Ok(())
    }

    /// Turns the share off.
    pub fn clear(&self) {
        *self.0.write().expect("mod share lock") = Arc::default();
    }

    pub fn manifest(&self) -> ModManifest {
        self.current().manifest.clone()
    }

    pub fn offer(&self) -> ModsOffer {
        self.current().manifest.offer()
    }

    fn current(&self) -> Arc<Shared> {
        self.0.read().expect("mod share lock").clone()
    }
}

impl fmt::Debug for ModShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ModShare({} mods)", self.current().manifest.mods.len())
    }
}

/// Guest request.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Manifest,
    File {
        blake3: String,
        /// From which byte to send (resume after a drop).
        #[serde(default)]
        offset: u64,
    },
}

/// The host's answer to a file request; after a successful answer the contents follow.
#[derive(Debug, Default, Serialize, Deserialize)]
struct FileReply {
    /// Full file size.
    #[serde(default)]
    size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// What the host did with the request.
#[derive(Debug, PartialEq, Eq)]
pub enum Served {
    /// Sent the mod list.
    Manifest { files: usize },
    /// Sent a file (`sent` is how many bytes went out: less than the size when resuming).
    File { file_name: String, sent: u64 },
    /// Refused: no such mod in the share or the file on disk changed.
    Refused(String),
}

/// Host: serves one guest request. `send`/`recv` are the stream after the type byte.
pub async fn serve<W, R>(send: &mut W, recv: &mut R, share: &ModShare) -> Result<Served>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let request: Request =
        tokio::time::timeout(REPLY_TIMEOUT, wire::read_msg_limited(recv, MAX_REQUEST_MSG))
            .await
            .context(tr!(
                "гость не прислал запрос модов",
                "the guest did not send a mod request"
            ))??;
    let shared = share.current();
    let served = match request {
        Request::Manifest => {
            wire::write_msg_limited(send, &shared.manifest, MAX_MANIFEST_MSG).await?;
            Served::Manifest {
                files: shared.manifest.mods.len(),
            }
        }
        Request::File { blake3, offset } => match open_shared(&shared, &blake3, offset).await {
            Ok((mut file, entry)) => {
                let reply = FileReply {
                    size: entry.size,
                    error: None,
                };
                wire::write_msg_limited(send, &reply, MAX_REQUEST_MSG).await?;
                let mut left = entry.size - offset;
                let mut buf = vec![0u8; CHUNK];
                while left > 0 {
                    let want = usize::try_from(left).map_or(CHUNK, |l| l.min(CHUNK));
                    let n = file.read(&mut buf[..want]).await?;
                    if n == 0 {
                        let name = &entry.file_name;
                        bail!(tr!(
                            format!("файл мода «{name}» стал короче во время передачи"),
                            format!("mod file \"{name}\" got shorter during the transfer")
                        ));
                    }
                    send.write_all(&buf[..n]).await.context(tr!(
                        "канал с гостем оборвался",
                        "the connection to the guest dropped"
                    ))?;
                    left -= n as u64;
                }
                Served::File {
                    file_name: entry.file_name,
                    sent: entry.size - offset,
                }
            }
            Err(reason) => {
                let reply = FileReply {
                    size: 0,
                    error: Some(reason.clone()),
                };
                wire::write_msg_limited(send, &reply, MAX_REQUEST_MSG).await?;
                Served::Refused(reason)
            }
        },
    };
    let _ = send.shutdown().await;
    Ok(served)
}

/// Opens a file from the share at the right position. The error is the rejection text for the guest (without
/// this PC's paths).
async fn open_shared(
    shared: &Shared,
    blake3: &str,
    offset: u64,
) -> Result<(fs::File, ModEntry), String> {
    let Some(found) = shared.files.get(blake3) else {
        return Err(tr!("такого мода нет в раздаче", "that mod is not being shared").into());
    };
    let entry = found.entry.clone();
    let changed = || {
        tr!(
            format!("мод «{}» изменился на ПК хоста", entry.file_name),
            format!("mod \"{}\" changed on the host's PC", entry.file_name)
        )
    };
    let mut file = fs::File::open(&found.path).await.map_err(|_| changed())?;
    let len = file.metadata().await.map_err(|_| changed())?.len();
    if len != entry.size {
        return Err(changed());
    }
    if offset > len {
        return Err(tr!(
            "докачка с места за концом файла",
            "resume offset is past the end of the file"
        )
        .into());
    }
    file.seek(SeekFrom::Start(offset))
        .await
        .map_err(|_| changed())?;
    Ok((file, entry))
}

// ------------------------------------------------------------------ guest

/// Guest: gets and checks the room's mod list.
pub async fn fetch_manifest(conn: &Connection) -> Result<ModManifest> {
    let (mut send, mut recv) = conn.open_bi().await.context(tr!(
        "не удалось открыть поток модов",
        "could not open the mods stream"
    ))?;
    wire::write_kind(&mut send, StreamKind::Mods).await?;
    request_manifest(&mut send, &mut recv).await
}

/// Same as [`fetch_manifest`], on a ready stream (after the type byte).
pub async fn request_manifest<W, R>(send: &mut W, recv: &mut R) -> Result<ModManifest>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    wire::write_msg_limited(send, &Request::Manifest, MAX_REQUEST_MSG).await?;
    let _ = send.shutdown().await;
    let manifest: ModManifest = tokio::time::timeout(
        REPLY_TIMEOUT,
        wire::read_msg_limited(recv, MAX_MANIFEST_MSG),
    )
    .await
    .context(tr!(
        "хост не прислал список модов",
        "the host did not send the mod list"
    ))??;
    manifest.validate().context(tr!(
        "хост прислал недопустимый список модов",
        "the host sent an invalid mod list"
    ))?;
    Ok(manifest)
}

/// Guest: downloads a mod from the host into a temporary `part` file and checks BLAKE3.
///
/// - If `part` already exists (the previous transfer dropped), resumes from the same place.
/// - `progress` gets the number of file bytes downloaded so far after each chunk.
/// - A "hash mismatch" error deletes `part`; on a connection drop `part` stays for
///   resuming. The finished file stays in `part`: the caller moves it into place.
pub async fn fetch_file(
    conn: &Connection,
    entry: &ModEntry,
    part: &Path,
    mut progress: impl FnMut(u64) + Send,
) -> Result<()> {
    let (mut send, mut recv) = conn.open_bi().await.context(tr!(
        "не удалось открыть поток модов",
        "could not open the mods stream"
    ))?;
    wire::write_kind(&mut send, StreamKind::Mods).await?;
    request_file(&mut send, &mut recv, entry, part, &mut progress).await
}

/// Same as [`fetch_file`], on a ready stream (after the type byte).
pub async fn request_file<W, R>(
    send: &mut W,
    recv: &mut R,
    entry: &ModEntry,
    part: &Path,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    entry.validate()?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(part)
        .await
        .with_context(|| {
            tr!(
                format!("не удалось создать {}", part.display()),
                format!("could not create {}", part.display())
            )
        })?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; CHUNK];

    // Resume: the hash of the already downloaded part is recomputed, so integrity is checked over
    // the whole file, not just the new part.
    let mut have = file.metadata().await?.len();
    if have > entry.size {
        file.set_len(0).await?;
        have = 0;
    }
    if have > 0 {
        loop {
            let n = file.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        progress(have);
    }
    if have == entry.size && hasher.finalize().to_hex().as_str() == entry.blake3 {
        return Ok(()); // the file is already fully downloaded
    }
    if have == entry.size {
        // Full size, but wrong contents: start over.
        file.set_len(0).await?;
        file.rewind().await?;
        hasher.reset();
        have = 0;
    }

    let request = Request::File {
        blake3: entry.blake3.clone(),
        offset: have,
    };
    wire::write_msg_limited(send, &request, MAX_REQUEST_MSG).await?;
    let _ = send.shutdown().await;
    let reply: FileReply =
        tokio::time::timeout(REPLY_TIMEOUT, wire::read_msg_limited(recv, MAX_REQUEST_MSG))
            .await
            .context(tr!(
                "хост не ответил на запрос мода",
                "the host did not answer the mod request"
            ))??;
    if let Some(error) = reply.error {
        if have == 0 {
            // Nothing downloaded: do not leave an empty file.
            drop(file);
            let _ = fs::remove_file(part).await;
        }
        // Text from the host is shown to the user, so sanitize it.
        let (name, why) = (&entry.file_name, wire::sanitize_text(&error, 200));
        bail!(tr!(
            format!("хост не отдал мод «{name}»: {why}"),
            format!("the host did not provide mod \"{name}\": {why}")
        ));
    }
    if reply.size != entry.size {
        let (name, got, want) = (&entry.file_name, reply.size, entry.size);
        bail!(tr!(
            format!("хост отдаёт мод «{name}» другого размера ({got} вместо {want} байт)"),
            format!(
                "the host serves mod \"{name}\" at a different size ({got} instead of {want} bytes)"
            )
        ));
    }

    loop {
        let n = tokio::time::timeout(CHUNK_TIMEOUT, recv.read(&mut buf))
            .await
            .map_err(|_| {
                anyhow!(tr!(
                    format!("хост перестал отправлять мод «{}»", entry.file_name),
                    format!("the host stopped sending mod \"{}\"", entry.file_name)
                ))
            })?
            .context(tr!(
                "канал с хостом оборвался",
                "the connection to the host dropped"
            ))?;
        if n == 0 {
            break;
        }
        have += n as u64;
        if have > entry.size {
            drop(file);
            let _ = fs::remove_file(part).await;
            let name = &entry.file_name;
            bail!(tr!(
                format!("хост прислал больше данных, чем размер мода «{name}»"),
                format!("the host sent more data than the size of mod \"{name}\"")
            ));
        }
        file.write_all(&buf[..n]).await.with_context(|| {
            tr!(
                format!("не удалось записать {}", part.display()),
                format!("could not write {}", part.display())
            )
        })?;
        hasher.update(&buf[..n]);
        progress(have);
    }
    file.flush().await?;
    if have != entry.size {
        let (name, want) = (&entry.file_name, entry.size);
        bail!(tr!(
            format!("передача мода «{name}» оборвалась: получено {have} из {want} байт"),
            format!(
                "the transfer of mod \"{name}\" was interrupted: received {have} of {want} bytes"
            )
        ));
    }
    if hasher.finalize().to_hex().as_str() != entry.blake3 {
        drop(file);
        let _ = fs::remove_file(part).await;
        let name = &entry.file_name;
        bail!(tr!(
            format!("мод «{name}» повреждён при передаче: хэш не совпал"),
            format!("mod \"{name}\" was corrupted in transfer: hash mismatch")
        ));
    }
    file.sync_all().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, data: &[u8]) -> ModEntry {
        ModEntry {
            file_name: name.into(),
            size: data.len() as u64,
            // The tunnel does not need the SHA-1 itself, only its shape.
            sha1: "0".repeat(40),
            blake3: blake3::hash(data).to_hex().to_string(),
        }
    }

    #[test]
    fn file_names_from_the_network_cannot_leave_the_mods_folder() {
        for good in [
            "sodium.jar",
            "Fabric API 0.92.2+1.20.1.jar",
            "мод [1.20].JAR",
        ] {
            assert!(validate_file_name(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            ".jar",
            "../evil.jar",
            "..\\evil.jar",
            "sub/evil.jar",
            "sub\\evil.jar",
            "/etc/evil.jar",
            "C:\\Windows\\evil.jar",
            "C:evil.jar",
            "evil.jar:stream.jar",
            "evil..jar",
            ".hidden.jar",
            "evil\0.jar",
            "evil\u{1b}[2J.jar",
            " evil.jar",
            "evil.jar ",
            "evil.exe",
            "evil.jar.exe",
            "evil.jar.",
            "what?.jar",
            "NUL.jar",
        ] {
            assert!(
                validate_file_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(validate_file_name(&format!("{}.jar", "x".repeat(197))).is_err());
    }

    #[test]
    fn manifest_validation_rejects_anything_suspicious() {
        let good = ModManifest {
            mods: vec![entry("a.jar", b"a"), entry("b.jar", b"b")],
            optional: vec!["f".repeat(64)],
        };
        good.validate().unwrap();
        assert_eq!(good.total_size(), 2);
        assert_eq!(
            good.offer(),
            ModsOffer {
                files: 2,
                total_size: 2
            }
        );

        let broken = |change: &dyn Fn(&mut ModManifest)| {
            let mut manifest = good.clone();
            change(&mut manifest);
            manifest.validate().is_err()
        };
        assert!(broken(&|m| m.mods[0].file_name = "../evil.jar".into()));
        assert!(!broken(&|m| m.mods[0].file_name = "A.JAR".into()));
        assert!(
            broken(&|m| m.mods[1].file_name = "A.JAR".into()),
            "the same name in a different case"
        );
        assert!(broken(&|m| m.mods[1].blake3 = m.mods[0].blake3.clone()));
        assert!(broken(&|m| m.mods[0].blake3 = "xyz".into()));
        assert!(broken(
            &|m| m.mods[0].blake3 = m.mods[0].blake3.to_uppercase()
        ));
        assert!(broken(&|m| m.mods[0].sha1 = "abc".into()));
        assert!(broken(&|m| m.mods[0].size = MAX_MOD_SIZE + 1));
        assert!(broken(&|m| m.optional = vec!["not a hash".into()]));
        assert!(broken(&|m| {
            m.mods = (0..=MAX_MODS)
                .map(|i| entry(&format!("{i}.jar"), i.to_string().as_bytes()))
                .collect()
        }));
    }

    #[test]
    fn share_drops_duplicates_and_refuses_bad_names() {
        let share = ModShare::new();
        assert_eq!(share.offer(), ModsOffer::default());
        let shared = |name: &str, data: &[u8]| SharedMod {
            entry: entry(name, data),
            path: PathBuf::from(name),
        };
        let client_only = blake3::hash(b"shader").to_hex().to_string();
        share
            .set(
                vec![
                    shared("a.jar", b"same"),
                    shared("a copy.jar", b"same"),
                    shared("b.jar", b"b"),
                ],
                vec![
                    client_only.clone(),
                    client_only.clone(),
                    entry("b.jar", b"b").blake3,
                ],
            )
            .unwrap();
        let manifest = share.manifest();
        assert_eq!(
            manifest
                .mods
                .iter()
                .map(|m| m.file_name.as_str())
                .collect::<Vec<_>>(),
            ["a.jar", "b.jar"]
        );
        assert_eq!(manifest.optional, [client_only]);

        // A bad name: the share does not change.
        assert!(
            share
                .set(vec![shared("..\\evil.jar", b"x")], Vec::new())
                .is_err()
        );
        assert_eq!(share.offer().files, 2);
        share.clear();
        assert_eq!(share.offer().files, 0);
    }

    #[tokio::test]
    async fn guest_rejects_a_manifest_with_path_traversal() {
        // The "host" is a foreign program: it answers with a list whose name leads out of the mods folder.
        let (mut guest_side, mut host_side) = tokio::io::duplex(64 * 1024);
        let evil = serde_json::json!({
            "mods": [{
                "file_name": "../evil.jar",
                "size": 4,
                "sha1": "0".repeat(40),
                "blake3": "0".repeat(64),
            }]
        });
        tokio::spawn(async move {
            let _: Request = wire::read_msg_limited(&mut host_side, MAX_REQUEST_MSG)
                .await
                .unwrap();
            wire::write_msg_limited(&mut host_side, &evil, MAX_MANIFEST_MSG)
                .await
                .unwrap();
        });
        let (mut recv, mut send) = tokio::io::split(&mut guest_side);
        let err = request_manifest(&mut send, &mut recv).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("недопустимый список"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn guest_never_asks_for_an_entry_with_a_bad_name() {
        let part =
            std::env::temp_dir().join(format!("phase-bad-entry-{}.part", std::process::id()));
        let _ = std::fs::remove_file(&part);
        let (mut guest_side, _host_side) = tokio::io::duplex(1024);
        let (mut recv, mut send) = tokio::io::split(&mut guest_side);
        let mut bad = entry("ok.jar", b"data");
        bad.file_name = "..\\..\\evil.jar".into();
        let result = request_file(&mut send, &mut recv, &bad, &part, &mut |_| {}).await;
        assert!(result.is_err());
        assert!(!part.exists(), "the file is not even created");
    }
}
