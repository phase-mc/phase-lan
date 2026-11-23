//! The Minecraft `mods` folder: where it is, which mods it holds and how to temporarily set the extra ones aside.
//!
//! **Where the folder is.** In order:
//!
//! 1. the running game: the `--gameDir <folder>` parameter in the Java process command line
//!    (any launcher passes it), and if there is no parameter, the process's working folder
//!    (that is how your own server is found too); inside it, the `mods` subfolder;
//! 2. a folder the user chose themselves;
//! 3. the standard `%APPDATA%\.minecraft\mods`.
//!
//! **Which mods.** Only `*.jar` files at the root of the folder (the game does not read subfolders). Two
//! hashes are computed for each: SHA-1 (used to look the mod up on Modrinth) and BLAKE3 (used to
//! compare contents). Hashes are cached by file name, size and modification time,
//! so a repeated scan of a large pack does not reread the files.
//!
//! **Guest backup.** The guest's extra mods are not deleted but moved to
//! `mods/.phase-backup/`. The same place keeps a list of files the
//! program installed itself. [`restore`] undoes the sync entirely: it removes what was installed and
//! returns the user's own. The old `mods/.lanbridge-backup/` folder is moved into the new one, and while
//! it cannot be moved it is read together with it.
//!
//! **File names.** Any name a path is built from first goes through
//! [`validate_file_name`]: the name comes over the network from the host, and it must not take a write
//! outside the `mods` folder.

use phase_lang::tr;
use std::{
    collections::HashMap,
    fs,
    io::{self, ErrorKind, Read},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::nickname::is_game_command_line;

/// Subfolder of `mods` for the guest's set-aside mods. The game does not read it: mods are loaded
/// only from the root of `mods`.
pub const BACKUP_DIR: &str = ".phase-backup";
/// Backup folder from before the program was renamed (LanBridge). On first access it
/// is renamed to [`BACKUP_DIR`]; if that fails, "Restore my mods" looks in both.
pub const LEGACY_BACKUP_DIR: &str = ".lanbridge-backup";
/// Ending of a file that is still downloading. The game does not load such files.
pub const PART_SUFFIX: &str = ".part";
/// Maximum length of a mod file name, in characters.
pub const NAME_MAX_CHARS: usize = 200;

/// List of files installed by the program (lives in [`BACKUP_DIR`]).
const INSTALLED_FILE: &str = "installed.json";
/// Windows device names: a file with such a name cannot be opened.
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

// ------------------------------------------------------------------ file names

/// The name ends with `.jar` (case-insensitive) and does not consist of the ending alone.
pub fn is_jar_name(name: &str) -> bool {
    name.len() > 4
        && name.is_char_boundary(name.len() - 4)
        && name[name.len() - 4..].eq_ignore_ascii_case(".jar")
}

/// The file is a Phase Companion server mod (`phase-companion-<loader>-<version>.jar`).
/// Only the host needs it: Mod Sync does not hand it out to guests.
pub fn is_companion_jar(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    is_jar_name(&name)
        && (name.starts_with("phase-companion") || name.starts_with("phase_companion"))
}

/// Validates a mod file name. Only a plain name like `something.jar` is allowed:
///
/// - no path separators (`/`, `\`), colon (drive, NTFS stream) or `..`: the name
///   cannot point to another folder or be an absolute path;
/// - no null bytes or other control characters, no characters forbidden in
///   Windows names;
/// - not hidden (does not start with a dot), no space at the edges, not a device name
///   Windows (`CON.jar`);
/// - any extension other than `.jar` is rejected.
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
    if !is_jar_name(name) {
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

// ------------------------------------------------------------------ where the mods folder is

/// Where the `mods` folder came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirSource {
    /// Folder of the game running now.
    RunningGame,
    /// The user chose the folder.
    Chosen,
    /// Standard `%APPDATA%\.minecraft\mods`.
    Default,
}

/// The `mods` folder the program works with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModsDir {
    pub path: PathBuf,
    pub source: DirSource,
    /// The folder exists (`false` means it does not exist yet: it will appear when mods are installed).
    pub exists: bool,
}

/// Value of a command-line parameter: `--flag value`, `--flag "value"`,
/// `--flag=value` or `"--flag=value with spaces"`.
pub fn flag_value(command_line: &str, flag: &str) -> Option<String> {
    let mut rest = command_line;
    while let Some(at) = rest.find(flag) {
        let before = rest[..at].chars().next_back();
        let after = &rest[at + flag.len()..];
        rest = after;
        if before.is_some_and(|c| !c.is_whitespace() && c != '"') {
            continue;
        }
        let value = if let Some(value) = after.strip_prefix('=') {
            // The whole parameter in quotes: the value runs to the closing quote.
            if before == Some('"') {
                value.split('"').next()
            } else {
                value.split(char::is_whitespace).next()
            }
        } else if after.starts_with(char::is_whitespace) {
            let value = after.trim_start();
            match value.strip_prefix('"') {
                Some(quoted) => quoted.split('"').next(),
                None => value.split(char::is_whitespace).next(),
            }
        } else {
            // Another parameter with the same start (`--gameDirectory`).
            continue;
        };
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            return Some(value.to_string());
        }
    }
    None
}

/// Game folder from the running game's command line: the `--gameDir` value.
pub fn game_dir_from_command_line(command_line: &str) -> Option<PathBuf> {
    flag_value(command_line, "--gameDir")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
}

/// The command line looks like a Minecraft server (it has no `--gameDir`).
fn is_server_command_line(command_line: &str) -> bool {
    let lower = command_line.to_lowercase();
    ["minecraft", "forge", "fabric", "quilt", "nogui"]
        .iter()
        .any(|word| lower.contains(word))
}

/// `mods` folders of running games: first clients with `--gameDir`, then processes whose
/// working folder contains `mods` (your own server). The call blocks.
pub fn running_mods_dirs() -> Vec<PathBuf> {
    let Ok(processes) = crate::game_process::game_processes() else {
        return Vec::new();
    };
    let (mut by_flag, mut by_cwd) = (Vec::new(), Vec::new());
    for process in &processes {
        let line = &process.command_line;
        if let Some(dir) = game_dir_from_command_line(line) {
            by_flag.push(dir.join("mods"));
        } else if (is_game_command_line(line) || is_server_command_line(line))
            && let Some(dir) = phase_netstat::process::working_dir(process.pid)
        {
            let mods = dir.join("mods");
            if mods.is_dir() {
                by_cwd.push(mods);
            }
        }
    }
    by_flag.append(&mut by_cwd);
    let mut seen = Vec::new();
    by_flag.retain(|dir| {
        let new = !seen.contains(dir);
        if new {
            seen.push(dir.clone());
        }
        new
    });
    by_flag
}

/// A Minecraft client is running on this PC now. The call blocks.
pub fn is_game_running() -> bool {
    crate::game_process::game_processes()
        .map(|processes| {
            processes
                .iter()
                .any(|p| is_game_command_line(&p.command_line))
        })
        .unwrap_or(false)
}

/// Standard mods folder: `%APPDATA%\.minecraft\mods`.
pub fn default_mods_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join(".minecraft").join("mods"))
}

/// Picks the `mods` folder from candidates (order: see the module description).
///
/// The running game's folder is taken only if it already has `mods`: a game without mods must not
/// override a folder the user chose themselves.
pub fn choose(
    running: &[PathBuf],
    chosen: Option<&Path>,
    default: Option<&Path>,
) -> Option<ModsDir> {
    if let Some(dir) = running.iter().find(|dir| dir.is_dir()) {
        return Some(ModsDir {
            path: dir.clone(),
            source: DirSource::RunningGame,
            exists: true,
        });
    }
    let (path, source) = match (chosen, running.first(), default) {
        (Some(chosen), _, _) => (chosen, DirSource::Chosen),
        // The game is running but has no mods folder yet: mods are needed by exactly this game.
        (None, Some(running), _) => (running.as_path(), DirSource::RunningGame),
        (None, None, Some(default)) => (default, DirSource::Default),
        (None, None, None) => return None,
    };
    Some(ModsDir {
        path: path.to_path_buf(),
        source,
        exists: path.is_dir(),
    })
}

/// The `mods` folder on this PC. `detect_running` means look for a running game (switched
/// off in autotests: a test must not depend on what is being played right now). The call blocks.
pub fn locate(chosen: Option<&Path>, detect_running: bool) -> Option<ModsDir> {
    let running = if detect_running {
        running_mods_dirs()
    } else {
        Vec::new()
    };
    choose(&running, chosen, default_mods_dir().as_deref())
}

// ------------------------------------------------------------------ list and hashes

/// A `.jar` file in the mods folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JarFile {
    pub file_name: String,
    pub size: u64,
    /// Modification time, nanoseconds since 1970.
    pub mtime_ns: u64,
}

/// `*.jar` files at the root of the folder (no subfolders), alphabetical. No folder means an empty list.
pub fn list_jars(dir: &Path) -> io::Result<Vec<JarFile>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut jars = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Ok(file_name) = entry.file_name().into_string() else {
            continue;
        };
        if !is_jar_name(&file_name) {
            continue;
        }
        // A folder named `something.jar` is not a mod.
        let Ok(meta) = fs::metadata(entry.path()) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |since| {
                u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
            });
        jars.push(JarFile {
            file_name,
            size: meta.len(),
            mtime_ns,
        });
    }
    jars.sort_by_cached_key(|jar| jar.file_name.to_lowercase());
    Ok(jars)
}

// ------------------------------------------------------------------ mod loader

/// Command line of the running game whose mods folder is `mods_dir` (by `--gameDir`).
/// Needed to learn the loader of the game running now: the game folder of TLauncher and
/// other launchers can be shared between Fabric and Forge, and traces of the previous loader in it
/// remain. The call blocks.
pub fn running_game_command_line(mods_dir: &Path) -> Option<String> {
    let game_dir = mods_dir.parent()?;
    let processes = crate::game_process::game_processes().ok()?;
    processes
        .into_iter()
        .find(|process| {
            game_dir_from_command_line(&process.command_line)
                .is_some_and(|dir| same_dir(&dir, game_dir))
        })
        .map(|process| process.command_line)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// Loader metadata files inside a mod jar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JarMetadata {
    /// `fabric.mod.json`: a Fabric (and Quilt) mod.
    pub fabric: bool,
    /// `META-INF/mods.toml`: a Forge (and NeoForge before 1.20.5) mod.
    pub forge: bool,
    /// `META-INF/neoforge.mods.toml`: a NeoForge 1.20.5+ mod.
    pub neoforge: bool,
}

/// The largest zip central directory we read (for mods, tens of kilobytes).
const MAX_ZIP_DIRECTORY: u64 = 8 * 1024 * 1024;
/// End of a zip: the EOCD record (22 bytes) and a comment up to 64 KB.
const EOCD_SEARCH: u64 = 22 + 65_535;

/// Which loader metadata files are in the jar. Only the zip central directory
/// is read (file names are not compressed there); the mod contents are not unpacked.
pub fn jar_metadata(path: &Path) -> io::Result<JarMetadata> {
    use std::io::{Seek, SeekFrom};

    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let tail_len = len.min(EOCD_SEARCH);
    file.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0; usize::try_from(tail_len).unwrap_or(0)];
    file.read_exact(&mut tail)?;
    let bad = || io::Error::new(ErrorKind::InvalidData, "not a zip");
    let eocd = tail
        .windows(4)
        .rposition(|w| w == b"PK\x05\x06")
        .filter(|at| at + 22 <= tail.len())
        .ok_or_else(bad)?;
    let u32_at =
        |at: usize| u32::from_le_bytes([tail[at], tail[at + 1], tail[at + 2], tail[at + 3]]);
    let dir_size = u64::from(u32_at(eocd + 12));
    let dir_offset = u64::from(u32_at(eocd + 16));
    if dir_size > MAX_ZIP_DIRECTORY || dir_offset.saturating_add(dir_size) > len {
        return Err(bad());
    }
    file.seek(SeekFrom::Start(dir_offset))?;
    let mut directory = vec![0; usize::try_from(dir_size).unwrap_or(0)];
    file.read_exact(&mut directory)?;
    Ok(directory_metadata(&directory))
}

/// Parsing of central directory records: by each record's file name.
fn directory_metadata(directory: &[u8]) -> JarMetadata {
    let mut meta = JarMetadata::default();
    let mut at = 0;
    while at + 46 <= directory.len() && directory[at..at + 4] == *b"PK\x01\x02" {
        let u16_at = |i: usize| usize::from(u16::from_le_bytes([directory[i], directory[i + 1]]));
        let (name_len, extra_len, comment_len) =
            (u16_at(at + 28), u16_at(at + 30), u16_at(at + 32));
        let Some(name) = directory.get(at + 46..at + 46 + name_len) else {
            break;
        };
        match name {
            b"fabric.mod.json" => meta.fabric = true,
            b"META-INF/mods.toml" => meta.forge = true,
            b"META-INF/neoforge.mods.toml" => meta.neoforge = true,
            _ => {}
        }
        at += 46 + name_len + extra_len + comment_len;
    }
    meta
}

/// Hashes of the file contents (lowercase hexadecimal digits).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hashes {
    /// SHA-1, 40 characters: this is how Modrinth looks mods up.
    pub sha1: String,
    /// BLAKE3, 64 characters: integrity check.
    pub blake3: String,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Hashes of in-memory data.
pub fn hash_bytes(data: &[u8]) -> Hashes {
    Hashes {
        sha1: hex(&Sha1::digest(data)),
        blake3: blake3::hash(data).to_hex().to_string(),
    }
}

/// Reads the whole file and computes both hashes. Returns the size and hashes.
pub fn hash_file(path: &Path) -> io::Result<(u64, Hashes)> {
    let mut file = fs::File::open(path)?;
    let (mut sha1, mut blake3) = (Sha1::new(), blake3::Hasher::new());
    let mut buf = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha1.update(&buf[..n]);
        blake3.update(&buf[..n]);
        size += n as u64;
    }
    Ok((
        size,
        Hashes {
            sha1: hex(&sha1.finalize()),
            blake3: blake3.finalize().to_hex().to_string(),
        },
    ))
}

/// A mod in the folder: name, size and hashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModFile {
    pub file_name: String,
    pub size: u64,
    pub sha1: String,
    pub blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cached {
    size: u64,
    mtime_ns: u64,
    sha1: String,
    blake3: String,
}

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    /// Folder → file name → hashes.
    #[serde(default)]
    dirs: HashMap<String, HashMap<String, Cached>>,
}

/// Hash cache: a file with the same name, size and modification time is not reread.
/// May be stored on disk, so a large pack is hashed once.
#[derive(Debug, Default)]
pub struct HashCache {
    dirs: HashMap<String, HashMap<String, Cached>>,
    file: Option<PathBuf>,
    dirty: bool,
    computed: u64,
}

impl HashCache {
    /// Cache for the program's runtime only.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Cache from a file. If the file is missing or damaged, an empty cache (hashes are recomputed).
    pub fn load(file: &Path) -> Self {
        let dirs = fs::read(file)
            .ok()
            .and_then(|data| serde_json::from_slice::<CacheFile>(&data).ok())
            .map(|saved| saved.dirs)
            .unwrap_or_default();
        Self {
            dirs,
            file: Some(file.to_path_buf()),
            dirty: false,
            computed: 0,
        }
    }

    /// Saves the cache to a file if it changed (and if a file is set).
    pub fn save(&mut self) -> Result<()> {
        let Some(file) = self.file.clone().filter(|_| self.dirty) else {
            return Ok(());
        };
        let data = serde_json::to_vec(&CacheFile {
            dirs: self.dirs.clone(),
        })?;
        write_atomic(&file, &data)?;
        self.dirty = false;
        Ok(())
    }

    /// How many files had to be read and hashed (the rest came from the cache).
    pub fn computed(&self) -> u64 {
        self.computed
    }
}

/// Writes the file entirely or does not change it at all (temporary file + replace).
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            tr!(
                format!("не удалось создать папку {}", parent.display()),
                format!("could not create folder {}", parent.display())
            )
        })?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp{}", std::process::id()));
    let tmp = path.with_file_name(name);
    fs::write(&tmp, data).with_context(|| {
        tr!(
            format!("не удалось записать {}", tmp.display()),
            format!("could not write {}", tmp.display())
        )
    })?;
    fs::rename(&tmp, path).with_context(|| {
        let _ = fs::remove_file(&tmp);
        tr!(
            format!("не удалось сохранить {}", path.display()),
            format!("could not save {}", path.display())
        )
    })
}

/// Mods of the folder with hashes. A file that could not be read (busy, no permissions)
/// is skipped with a log entry. The call blocks.
pub fn scan(dir: &Path, cache: &mut HashCache) -> io::Result<Vec<ModFile>> {
    let jars = list_jars(dir)?;
    let key = dir.to_string_lossy().into_owned();
    let known = cache.dirs.remove(&key).unwrap_or_default();
    let mut fresh = HashMap::with_capacity(jars.len());
    let mut mods = Vec::with_capacity(jars.len());
    for jar in jars {
        let cached = known
            .get(&jar.file_name)
            .filter(|c| c.size == jar.size && c.mtime_ns == jar.mtime_ns)
            .cloned();
        let entry = match cached {
            Some(entry) => entry,
            None => match hash_file(&dir.join(&jar.file_name)) {
                Ok((size, hashes)) => {
                    cache.computed += 1;
                    cache.dirty = true;
                    Cached {
                        size,
                        mtime_ns: jar.mtime_ns,
                        sha1: hashes.sha1,
                        blake3: hashes.blake3,
                    }
                }
                Err(err) => {
                    tracing::warn!("мод {} не прочитан: {err}", jar.file_name);
                    continue;
                }
            },
        };
        mods.push(ModFile {
            file_name: jar.file_name.clone(),
            size: entry.size,
            sha1: entry.sha1.clone(),
            blake3: entry.blake3.clone(),
        });
        fresh.insert(jar.file_name, entry);
    }
    // Entries for deleted files do not stay in the cache.
    if fresh.len() != known.len() {
        cache.dirty = true;
    }
    cache.dirs.insert(key, fresh);
    Ok(mods)
}

// ------------------------------------------------------------------ install and backup

/// Backup folder inside the mods folder. New files always go here.
///
/// The old [`LEGACY_BACKUP_DIR`] folder is renamed to the new one if the new one does not exist yet.
/// If that fails (the folder is busy or the new one has appeared), the old one stays, and
/// [`backup_info`] and [`restore`] take it into account.
pub fn backup_dir(mods_dir: &Path) -> PathBuf {
    let dir = mods_dir.join(BACKUP_DIR);
    let legacy = mods_dir.join(LEGACY_BACKUP_DIR);
    if !dir.exists() && legacy.is_dir() {
        let _ = fs::rename(&legacy, &dir);
    }
    dir
}

/// Where backup files may be: the new folder and, until it is moved, the old one.
fn backup_dirs(mods_dir: &Path) -> [PathBuf; 2] {
    [backup_dir(mods_dir), mods_dir.join(LEGACY_BACKUP_DIR)]
}

/// Where a mod is downloaded until it is verified: `mods/<name>.jar.part`.
pub fn part_path(mods_dir: &Path, file_name: &str) -> Result<PathBuf> {
    validate_file_name(file_name)?;
    Ok(mods_dir.join(format!("{file_name}{PART_SUFFIX}")))
}

/// A file installed by the program.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Installed {
    file_name: String,
    size: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct InstalledFile {
    #[serde(default)]
    installed: Vec<Installed>,
}

/// What the program installed (across both backup folders). The file could have been edited by hand:
/// entries with bad names are discarded.
fn load_installed(mods_dir: &Path) -> Vec<Installed> {
    let mut all: Vec<Installed> = Vec::new();
    for dir in backup_dirs(mods_dir) {
        let entries = fs::read(dir.join(INSTALLED_FILE))
            .ok()
            .and_then(|data| serde_json::from_slice::<InstalledFile>(&data).ok())
            .map(|saved| saved.installed)
            .unwrap_or_default();
        for entry in entries {
            let known = all
                .iter()
                .any(|seen| seen.file_name.eq_ignore_ascii_case(&entry.file_name));
            if !known && validate_file_name(&entry.file_name).is_ok() {
                all.push(entry);
            }
        }
    }
    all
}

/// Writes the list to the new backup folder. The list from the old folder has already been merged into it
/// (see [`load_installed`]), so the old file is deleted.
fn save_installed(mods_dir: &Path, installed: &[Installed]) -> Result<()> {
    let legacy = mods_dir.join(LEGACY_BACKUP_DIR);
    if fs::remove_file(legacy.join(INSTALLED_FILE)).is_ok() {
        let _ = fs::remove_dir(&legacy);
    }
    let file = backup_dir(mods_dir).join(INSTALLED_FILE);
    if installed.is_empty() {
        match fs::remove_file(&file) {
            Ok(()) => {}
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).with_context(|| {
                    tr!(
                        format!("не удалось удалить {}", file.display()),
                        format!("could not delete {}", file.display())
                    )
                });
            }
        }
        // An empty backup folder is not needed (a non-empty one will not be deleted, that is not an error).
        let _ = fs::remove_dir(backup_dir(mods_dir));
        return Ok(());
    }
    let data = serde_json::to_vec_pretty(&InstalledFile {
        installed: installed.to_vec(),
    })?;
    write_atomic(&file, &data)
}

/// A verified mod takes its place: `mods/<name>.jar.part` → `mods/<name>.jar`.
/// The file is written to the list of files installed by the program.
pub fn install(mods_dir: &Path, file_name: &str) -> Result<PathBuf> {
    let part = part_path(mods_dir, file_name)?;
    let dest = mods_dir.join(file_name);
    let size = fs::metadata(&part)
        .with_context(|| {
            tr!(
                format!("скачанный файл {} пропал", part.display()),
                format!("the downloaded file {} disappeared", part.display())
            )
        })?
        .len();
    if dest.exists() {
        fs::remove_file(&dest).with_context(|| {
            tr!(
                format!(
                    "не удалось заменить {} (файл занят? Закройте Minecraft)",
                    dest.display()
                ),
                format!(
                    "could not replace {} (file in use? Close Minecraft)",
                    dest.display()
                )
            )
        })?;
    }
    fs::rename(&part, &dest).with_context(|| {
        tr!(
            format!("не удалось сохранить мод {}", dest.display()),
            format!("could not save the mod {}", dest.display())
        )
    })?;
    let mut installed = load_installed(mods_dir);
    installed.retain(|entry| !entry.file_name.eq_ignore_ascii_case(file_name));
    installed.push(Installed {
        file_name: file_name.to_string(),
        size,
    });
    save_installed(mods_dir, &installed)?;
    Ok(dest)
}

/// What happened to the extra mods.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StashReport {
    /// Moved to the backup (the user's own mods).
    pub moved: Vec<String>,
    /// Deleted: the program installed them earlier, they do not belong to the user.
    pub removed: Vec<String>,
    /// Untouched: the file is busy (usually by the running game). Name and reason.
    pub failed: Vec<(String, String)>,
}

/// A free name in the backup folder: `mod.jar`, and if taken, `mod (2).jar` and so on.
fn free_name(dir: &Path, file_name: &str) -> PathBuf {
    let first = dir.join(file_name);
    if !first.exists() {
        return first;
    }
    let stem = &file_name[..file_name.len() - 4];
    (2..1000)
        .map(|n| dir.join(format!("{stem} ({n}).jar")))
        .find(|candidate| !candidate.exists())
        .unwrap_or(first)
}

/// Removes the extra mods from the folder. The user's own mods are **not deleted** but moved
/// to `mods/.phase-backup/`; files the program installed earlier are deleted.
/// A busy file stays in place (see [`StashReport::failed`]).
///
/// All names are validated at once: on the first bad name no file is touched.
pub fn stash(mods_dir: &Path, file_names: &[String]) -> Result<StashReport> {
    for name in file_names {
        validate_file_name(name)?;
    }
    let backup = backup_dir(mods_dir);
    let mut installed = load_installed(mods_dir);
    let mut report = StashReport::default();
    for name in file_names {
        let src = mods_dir.join(name);
        let Ok(meta) = fs::metadata(&src) else {
            continue; // the file is already gone
        };
        if !meta.is_file() {
            continue;
        }
        let ours = installed.iter().position(|entry| {
            entry.file_name.eq_ignore_ascii_case(name) && entry.size == meta.len()
        });
        let result = match ours {
            Some(at) => fs::remove_file(&src).map(|()| {
                installed.remove(at);
                report.removed.push(name.clone());
            }),
            None => fs::create_dir_all(&backup)
                .and_then(|()| fs::rename(&src, free_name(&backup, name)))
                .map(|()| report.moved.push(name.clone())),
        };
        if let Err(err) = result {
            report.failed.push((name.clone(), err.to_string()));
        }
    }
    save_installed(mods_dir, &installed)?;
    Ok(report)
}

/// What is in the backup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackupInfo {
    /// How many of the user's own mods are in the backup.
    pub files: usize,
    /// How many files in the mods folder were installed by the program.
    pub installed: usize,
}

impl BackupInfo {
    /// There is something to restore: mods in the backup or files installed by the program.
    pub fn can_restore(&self) -> bool {
        self.files > 0 || self.installed > 0
    }
}

pub fn backup_info(mods_dir: &Path) -> BackupInfo {
    BackupInfo {
        files: backup_dirs(mods_dir)
            .iter()
            .map(|dir| list_jars(dir).map_or(0, |jars| jars.len()))
            .sum(),
        installed: load_installed(mods_dir).len(),
    }
}

/// Result of restoring mods.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Returned from the backup to the mods folder.
    pub restored: Vec<String>,
    /// Deleted: host mods installed by the program.
    pub removed: Vec<String>,
    /// Left in the backup: the mods folder already has another file with that name.
    pub conflicts: Vec<String>,
    /// Untouched: the file is busy (usually by the running game). Name and reason.
    pub failed: Vec<(String, String)>,
}

/// "Restore my mods": undoes the sync. Files installed by the program
/// are deleted (if the user has not replaced them), mods from `mods/.phase-backup/` (and the old
/// `mods/.lanbridge-backup/` if it is left)
/// are returned to the mods folder. A foreign file with the same name is not overwritten.
pub fn restore(mods_dir: &Path) -> Result<RestoreReport> {
    let mut report = RestoreReport::default();
    let mut kept = Vec::new();
    for entry in load_installed(mods_dir) {
        let path = mods_dir.join(&entry.file_name);
        match fs::metadata(&path) {
            // The same size means it is our file. A different one means the user replaced it: we do not touch it.
            Ok(meta) if meta.is_file() && meta.len() == entry.size => {
                match fs::remove_file(&path) {
                    Ok(()) => report.removed.push(entry.file_name),
                    Err(err) => {
                        report
                            .failed
                            .push((entry.file_name.clone(), err.to_string()));
                        kept.push(entry);
                    }
                }
            }
            _ => {}
        }
    }
    for backup in backup_dirs(mods_dir) {
        if !backup.exists() {
            continue;
        }
        let jars = list_jars(&backup).with_context(|| {
            tr!(
                format!("не удалось прочитать {}", backup.display()),
                format!("could not read {}", backup.display())
            )
        })?;
        for jar in jars {
            if validate_file_name(&jar.file_name).is_err() {
                continue;
            }
            let dest = mods_dir.join(&jar.file_name);
            if dest.exists() {
                report.conflicts.push(jar.file_name);
                continue;
            }
            match fs::rename(backup.join(&jar.file_name), &dest) {
                Ok(()) => report.restored.push(jar.file_name),
                Err(err) => report.failed.push((jar.file_name, err.to_string())),
            }
        }
    }
    save_installed(mods_dir, &kept)?;
    // The old folder is empty: remove it (a non-empty one will not be deleted, that is not an error).
    let _ = fs::remove_dir(mods_dir.join(LEGACY_BACKUP_DIR));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_jar_is_recognised() {
        assert!(is_companion_jar("phase-companion-fabric-26.3-0.1.0.jar"));
        assert!(is_companion_jar("Phase_Companion.JAR"));
        assert!(!is_companion_jar("phase-companion.txt"));
        assert!(!is_companion_jar("sodium-fabric-0.6.jar"));
    }

    #[test]
    fn flag_value_forms() {
        let value = |line: &str| flag_value(line, "--gameDir");
        assert_eq!(
            value(r"javaw --gameDir C:\mc --x").as_deref(),
            Some(r"C:\mc")
        );
        assert_eq!(
            value(r#"javaw --gameDir "C:\My Games\mc" --x"#).as_deref(),
            Some(r"C:\My Games\mc")
        );
        assert_eq!(
            value(r"javaw --gameDir=C:\mc --x").as_deref(),
            Some(r"C:\mc")
        );
        assert_eq!(
            value(r#"javaw "--gameDir=C:\My Games\mc" --x"#).as_deref(),
            Some(r"C:\My Games\mc")
        );
        // A similar parameter and text inside another word are not it.
        assert_eq!(value(r"javaw --gameDirectory C:\mc"), None);
        assert_eq!(value(r"javaw x--gameDir C:\mc"), None);
        assert_eq!(value("javaw --gameDir"), None);
    }

    /// A minimal zip without compression: only file names, empty contents.
    fn tiny_zip(names: &[&str]) -> Vec<u8> {
        let (mut out, mut dir) = (Vec::new(), Vec::new());
        for name in names {
            let offset = u32::try_from(out.len()).unwrap();
            let len = u16::try_from(name.len()).unwrap();
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&[0; 12]);
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            dir.extend_from_slice(b"PK\x01\x02");
            dir.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            dir.extend_from_slice(&[0; 12]);
            dir.extend_from_slice(&len.to_le_bytes());
            dir.extend_from_slice(&[0; 12]);
            dir.extend_from_slice(&offset.to_le_bytes());
            dir.extend_from_slice(name.as_bytes());
        }
        let dir_offset = u32::try_from(out.len()).unwrap();
        let dir_size = u32::try_from(dir.len()).unwrap();
        let count = u16::try_from(names.len()).unwrap();
        out.extend_from_slice(&dir);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&dir_size.to_le_bytes());
        out.extend_from_slice(&dir_offset.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn jar_metadata_reads_loader_files() {
        let dir = std::env::temp_dir().join(format!("phase-jar-meta-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, entries: &[&str]| {
            let path = dir.join(name);
            fs::write(&path, tiny_zip(entries)).unwrap();
            jar_metadata(&path).unwrap()
        };
        let forge = write(
            "a.jar",
            &["META-INF/MANIFEST.MF", "META-INF/mods.toml", "a/B.class"],
        );
        assert_eq!(
            forge,
            JarMetadata {
                forge: true,
                ..JarMetadata::default()
            }
        );
        let neo = write("b.jar", &["META-INF/neoforge.mods.toml"]);
        assert_eq!(
            neo,
            JarMetadata {
                neoforge: true,
                ..JarMetadata::default()
            }
        );
        let fabric = write(
            "c.jar",
            &["fabric.mod.json", "assets/x/fabric.mod.json.bak"],
        );
        assert_eq!(
            fabric,
            JarMetadata {
                fabric: true,
                ..JarMetadata::default()
            }
        );
        // A name inside another folder is not mod metadata.
        let nested = write("d.jar", &["data/META-INF/mods.toml"]);
        assert_eq!(nested, JarMetadata::default());
        fs::write(dir.join("e.jar"), b"not a zip").unwrap();
        assert!(jar_metadata(&dir.join("e.jar")).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn same_dir_ignores_case_and_slashes() {
        assert!(same_dir(
            Path::new(r"C:\Games\MC\"),
            Path::new("c:/games/mc")
        ));
        assert!(!same_dir(
            Path::new(r"C:\Games\MC2"),
            Path::new(r"C:\Games\MC")
        ));
    }

    #[test]
    fn free_name_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("phase-free-name-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_name(&dir, "a.jar"), dir.join("a.jar"));
        fs::write(dir.join("a.jar"), b"1").unwrap();
        assert_eq!(free_name(&dir, "a.jar"), dir.join("a (2).jar"));
        fs::write(dir.join("a (2).jar"), b"2").unwrap();
        assert_eq!(free_name(&dir, "a.jar"), dir.join("a (3).jar"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
