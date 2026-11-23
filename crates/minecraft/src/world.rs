//! The host's active world: the `saves/<world>` folder the game currently holds open.
//!
//! **How we look.**
//!
//! 1. The game folder is the `--gameDir` parameter of the process that listens on the game port (so with
//!    two running games the right one is taken). If the port is listened on by something that is not the game, no world is searched
//!    at all: Phase does not write into a foreign world.
//! 2. Among `saves/*` the open world is the one whose `session.lock` is locked: the game (since 1.16) holds
//!    a lock on it the whole time the world is open. The check is an attempt to take the lock
//!    without waiting; the file is not changed.
//! 3. Your own server is started without `--gameDir`: its world is the `level-name` folder from
//!    `server.properties` in the process's working folder.
//!
//! If several worlds are open, the one whose folder name is in the LAN world's name is taken,
//! otherwise the last opened.
//!
//! **World open time** is the modification time of `session.lock`: the game rewrites it on
//! every open. It shows whether the game has had time to load the datapack: functions are read
//! only when the world is opened and on `/reload`.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

use flate2::read::GzDecoder;

use crate::{
    datapack::{COMPANION_ALIVE_FILE, COMPANION_FOLDER},
    mods::game_dir_from_command_line,
};

/// World lock file.
const SESSION_LOCK: &str = "session.lock";

/// A Minecraft world on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct World {
    /// World folder (`…/saves/My world`).
    pub dir: PathBuf,
    /// World folder name.
    pub name: String,
    /// The game holds the world open right now.
    pub open: bool,
    /// When the world was last opened.
    pub opened: Option<SystemTime>,
}

impl World {
    /// The world in folder `dir` (`None` means it is not a world folder: it has no `level.dat`).
    pub fn at(dir: &Path) -> Option<Self> {
        if !dir.join("level.dat").is_file() {
            return None;
        }
        let lock = dir.join(SESSION_LOCK);
        Some(Self {
            name: dir.file_name()?.to_string_lossy().into_owned(),
            open: is_locked(&lock),
            opened: fs::metadata(&lock).and_then(|meta| meta.modified()).ok(),
            dir: dir.to_path_buf(),
        })
    }

    /// The world's datapacks folder.
    pub fn datapacks_dir(&self) -> PathBuf {
        self.dir.join("datapacks")
    }

    /// Phase folder in the world (files for Phase Companion).
    pub fn companion_dir(&self) -> PathBuf {
        self.dir.join(COMPANION_FOLDER)
    }

    /// The Phase Companion mod works in the world: it creates a confirmation file on every
    /// world open, so a file older than the world's opening does not count.
    pub fn companion_alive(&self) -> bool {
        let alive = fs::metadata(self.companion_dir().join(COMPANION_ALIVE_FILE))
            .and_then(|meta| meta.modified())
            .ok();
        match (alive, self.opened) {
            (Some(alive), Some(opened)) => self.open && alive >= opened,
            _ => false,
        }
    }
}

/// The file is locked by another program (the game holds the world open). If the file is missing or
/// cannot be opened, it is not locked.
pub fn is_locked(lock_file: &Path) -> bool {
    let Ok(file) = fs::File::open(lock_file) else {
        return false;
    };
    match file.try_lock_shared() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(fs::TryLockError::WouldBlock) => true,
        Err(fs::TryLockError::Error(_)) => false,
    }
}

/// Worlds in the game folder: `<game folder>/saves/*`.
pub fn worlds_in(game_dir: &Path) -> Vec<World> {
    let Ok(entries) = fs::read_dir(game_dir.join("saves")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| World::at(&entry.path()))
        .collect()
}

/// The largest world icon we give to the window (the game's is 64×64, usually 5–15 KB).
pub const MAX_ICON_BYTES: u64 = 64 * 1024;

/// A recent world for the list in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentWorld {
    /// Display name: `LevelName` from `level.dat`, without `§x`; if it is missing, the folder name.
    pub name: String,
    /// World folder name.
    pub folder: String,
    /// The game holds the world open right now.
    pub open: bool,
    /// When the world was last opened (ms since 1970), 0 means unknown.
    pub opened_ms: u64,
    /// PNG of the `icon.png` icon; `None` means the file is missing or larger than [`MAX_ICON_BYTES`].
    pub icon: Option<Vec<u8>>,
}

/// Recent worlds from game folders: open ones first, then by open time (newer
/// first), at most `limit`. A folder named twice is taken once.
pub fn recent_in(game_dirs: &[PathBuf], limit: usize) -> Vec<RecentWorld> {
    let mut worlds: Vec<World> = Vec::new();
    for game_dir in game_dirs {
        for world in worlds_in(game_dir) {
            if !worlds.iter().any(|known| known.dir == world.dir) {
                worlds.push(world);
            }
        }
    }
    worlds.sort_by_key(|world| (!world.open, std::cmp::Reverse(world.opened)));
    worlds
        .into_iter()
        .take(limit)
        .map(|world| RecentWorld {
            icon: read_icon(&world.dir.join("icon.png")),
            opened_ms: world
                .opened
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_millis() as u64),
            name: world_title(&world.dir, &world.name),
            folder: world.name,
            open: world.open,
        })
        .collect()
}

/// Recent worlds of the standard game folder and the folders of running games. The call blocks.
pub fn recent_worlds(limit: usize) -> Vec<RecentWorld> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("APPDATA")
        .map(|appdata| PathBuf::from(appdata).join(".minecraft"))
        .into_iter()
        .collect();
    // `running_mods_dirs` gives `<game folder>/mods`.
    dirs.extend(
        crate::mods::running_mods_dirs()
            .iter()
            .filter_map(|mods| mods.parent().map(Path::to_path_buf)),
    );
    recent_in(&dirs, limit)
}

/// PNG of the world icon if the file exists and is no larger than [`MAX_ICON_BYTES`].
fn read_icon(path: &Path) -> Option<Vec<u8>> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ICON_BYTES {
        return None;
    }
    fs::read(path).ok()
}

/// Limit of the unpacked `level.dat` (real worlds have tens of KB).
const MAX_LEVEL_DAT_BYTES: u64 = 16 * 1024 * 1024;

/// NBT nesting depth we parse: on broken files we do not go into deep recursion.
const MAX_NBT_DEPTH: usize = 512;

/// Display name: `LevelName` from `level.dat` without `§x` color codes. If it is missing
/// or empty, the folder name (also without `§x`).
pub fn world_title(dir: &Path, folder: &str) -> String {
    match level_name(dir).map(|name| strip_color_codes(&name)) {
        Some(name) if !name.is_empty() => name,
        _ => strip_color_codes(folder),
    }
}

/// `Data.LevelName` from `level.dat`. `None` if the file is missing, cannot be unpacked, is
/// larger than [`MAX_LEVEL_DAT_BYTES`], fails to parse or the field is absent.
fn level_name(dir: &Path) -> Option<String> {
    let file = fs::File::open(dir.join("level.dat")).ok()?;
    let mut raw = Vec::new();
    GzDecoder::new(file)
        .take(MAX_LEVEL_DAT_BYTES + 1)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() as u64 > MAX_LEVEL_DAT_BYTES {
        return None;
    }
    let mut nbt = Nbt { data: &raw, pos: 0 };
    // The root is a compound (its name does not matter), inside it `Data` is a compound with `LevelName`.
    if nbt.byte()? != TAG_COMPOUND {
        return None;
    }
    nbt.string_bytes()?;
    if nbt.find_tag(b"Data")? != TAG_COMPOUND {
        return None;
    }
    if nbt.find_tag(b"LevelName")? != TAG_STRING {
        return None;
    }
    nbt.string_bytes().map(decode_mutf8)
}

const TAG_END: u8 = 0;
const TAG_STRING: u8 = 8;
const TAG_COMPOUND: u8 = 10;

/// NBT reader only for searching a compound. All reads check bounds: a broken file
/// gives `None`, not a panic.
struct Nbt<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Nbt<'_> {
    fn bytes(&mut self, len: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(len)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn skip_bytes(&mut self, len: usize) -> Option<()> {
        self.bytes(len)?;
        Some(())
    }

    fn byte(&mut self) -> Option<u8> {
        self.bytes(1).map(|slice| slice[0])
    }

    /// Length of an array or list (`i32`, not less than zero).
    fn count(&mut self) -> Option<usize> {
        let raw: [u8; 4] = self.bytes(4)?.try_into().ok()?;
        usize::try_from(i32::from_be_bytes(raw)).ok()
    }

    /// NBT string: `u16` length and MUTF-8 bytes.
    fn string_bytes(&mut self) -> Option<&[u8]> {
        let raw: [u8; 2] = self.bytes(2)?.try_into().ok()?;
        let len = usize::from(u16::from_be_bytes(raw));
        self.bytes(len)
    }

    /// Walks the compound's entries to the tag named `key`. Returns its type; the cursor stands
    /// on that tag's data. `None` means the tag is missing or the data is broken.
    fn find_tag(&mut self, key: &[u8]) -> Option<u8> {
        loop {
            let tag = self.byte()?;
            if tag == TAG_END {
                return None;
            }
            let name = self.string_bytes()?;
            if name == key {
                return Some(tag);
            }
            self.skip(tag, 1)?;
        }
    }

    /// Skips the data of a tag of type `tag`.
    fn skip(&mut self, tag: u8, depth: usize) -> Option<()> {
        if depth > MAX_NBT_DEPTH {
            return None;
        }
        match tag {
            1 => self.skip_bytes(1),     // Byte
            2 => self.skip_bytes(2),     // Short
            3 | 5 => self.skip_bytes(4), // Int, Float
            4 | 6 => self.skip_bytes(8), // Long, Double
            7 => {
                let len = self.count()?; // ByteArray
                self.skip_bytes(len)
            }
            TAG_STRING => {
                self.string_bytes()?;
                Some(())
            }
            9 => {
                // List: element type, count, then the elements themselves.
                let elem = self.byte()?;
                let len = self.count()?;
                if elem == TAG_END && len > 0 {
                    return None;
                }
                for _ in 0..len {
                    self.skip(elem, depth + 1)?;
                }
                Some(())
            }
            TAG_COMPOUND => loop {
                let child = self.byte()?;
                if child == TAG_END {
                    return Some(());
                }
                self.string_bytes()?;
                self.skip(child, depth + 1)?;
            },
            11 => {
                let len = self.count()?; // IntArray
                self.skip_bytes(len.checked_mul(4)?)
            }
            12 => {
                let len = self.count()?; // LongArray
                self.skip_bytes(len.checked_mul(8)?)
            }
            _ => None,
        }
    }
}

/// A Minecraft string: MUTF-8 (surrogate pairs instead of 4-byte characters, `C0 80` instead of
/// `\0`). Broken bytes are replaced, no panic.
fn decode_mutf8(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (unit, step) = if (b & 0x80) == 0 {
            (u16::from(b), 1)
        } else if (b & 0xE0) == 0xC0 && i + 1 < bytes.len() {
            let low = u16::from(bytes[i + 1] & 0x3F);
            ((u16::from(b & 0x1F) << 6) | low, 2)
        } else if (b & 0xF0) == 0xE0 && i + 2 < bytes.len() {
            let mid = u16::from(bytes[i + 1] & 0x3F);
            let low = u16::from(bytes[i + 2] & 0x3F);
            ((u16::from(b & 0x0F) << 12) | (mid << 6) | low, 3)
        } else {
            (0xFFFD, 1)
        };
        units.push(unit);
        i += step;
    }
    String::from_utf16_lossy(&units)
}

/// Removes `§x` color codes (the sign and the character after it) and the string's edges.
fn strip_color_codes(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out.trim().to_owned()
}

/// The world of your own server in its working folder: `level-name` from `server.properties`
/// (`world` by default).
pub fn server_world(server_dir: &Path) -> Option<World> {
    let properties = fs::read_to_string(server_dir.join("server.properties")).ok()?;
    let level = properties
        .lines()
        .filter_map(|line| line.trim().strip_prefix("level-name"))
        .filter_map(|rest| rest.trim_start().strip_prefix('='))
        .map(str::trim)
        .find(|name| !name.is_empty())
        .unwrap_or("world");
    // The world name is a single folder inside the server folder, not a path anywhere.
    if level.contains(['/', '\\', ':']) || level.contains("..") {
        return None;
    }
    World::at(&server_dir.join(level))
}

/// Picks the active world from those found: the ones open now; among several, the one whose name
/// is in the LAN world's name (`motd`), otherwise the last opened.
pub fn choose(worlds: Vec<World>, motd: Option<&str>) -> Option<World> {
    let mut open: Vec<World> = worlds.into_iter().filter(|world| world.open).collect();
    open.sort_by_key(|world| std::cmp::Reverse(world.opened));
    let named = motd.and_then(|motd| {
        let motd = motd.to_lowercase();
        open.iter()
            .position(|world| motd.contains(&world.name.to_lowercase()))
    });
    match named {
        Some(at) => Some(open.swap_remove(at)),
        None => open.into_iter().next(),
    }
}

/// The active world and the game that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWorld {
    pub world: World,
    /// The nickname the host's game runs under (`--username`), if it is a client.
    pub username: Option<String>,
}

/// The host's active world: the world opened by **the process that listens on the game port**.
/// Phase writes the datapack only there. `None` means the port is listened on by something that is not the game (or several
/// programs at once), the world is closed, or it is a version before 1.16, which does not lock the world.
/// The call blocks.
pub fn find_active(game_port: u16, motd: Option<&str>) -> Option<ActiveWorld> {
    let owner = phase_netstat::tcp_listener_owner(game_port)
        .ok()
        .flatten()?;
    let processes = crate::game_process::game_processes().ok()?;
    let game = processes.iter().find(|process| process.pid == owner)?;
    let mut worlds = Vec::new();
    if let Some(game_dir) = game_dir_from_command_line(&game.command_line) {
        worlds.extend(worlds_in(&game_dir));
    } else if let Some(cwd) = phase_netstat::process::working_dir(game.pid) {
        // Your own server; a game without `--gameDir` also works in its own working folder.
        worlds.extend(server_world(&cwd));
        worlds.extend(worlds_in(&cwd));
    }
    Some(ActiveWorld {
        world: choose(worlds, motd)?,
        username: crate::nickname::from_command_line(&game.command_line),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::{io::Write, time::Duration};

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("phase-test-world-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_world(saves: &Path, name: &str) -> PathBuf {
        let dir = saves.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("level.dat"), b"nbt").unwrap();
        fs::write(dir.join(SESSION_LOCK), "\u{2603}").unwrap();
        dir
    }

    /// Holds the world "open" like the game: an exclusive `session.lock` lock.
    fn hold(world: &Path) -> fs::File {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(world.join(SESSION_LOCK))
            .unwrap();
        file.try_lock().unwrap();
        file
    }

    #[test]
    fn open_world_is_the_one_with_a_held_session_lock() {
        let game = temp_dir("lock");
        let saves = game.join("saves");
        let closed = make_world(&saves, "Old world");
        let opened = make_world(&saves, "Мой мир");
        // Not worlds: a folder without level.dat and a stray file.
        fs::create_dir_all(saves.join("not-a-world")).unwrap();
        fs::write(saves.join("readme.txt"), b"x").unwrap();

        let worlds = worlds_in(&game);
        assert_eq!(worlds.len(), 2);
        assert!(worlds.iter().all(|world| !world.open));
        assert_eq!(choose(worlds, None), None, "no world is open");

        let held = hold(&opened);
        assert!(is_locked(&opened.join(SESSION_LOCK)));
        assert!(!is_locked(&closed.join(SESSION_LOCK)));
        let found = choose(worlds_in(&game), Some("Steve - Мой мир")).unwrap();
        assert_eq!(found.dir, opened);
        assert_eq!(found.name, "Мой мир");
        assert!(found.open && found.opened.is_some());
        assert_eq!(found.datapacks_dir(), opened.join("datapacks"));

        // The check breaks nothing: the game's lock is in place, the world closed, free again.
        assert!(is_locked(&opened.join(SESSION_LOCK)));
        drop(held);
        assert!(!is_locked(&opened.join(SESSION_LOCK)));
        assert!(!is_locked(&game.join("нет такого файла")));
        assert_eq!(worlds_in(&game.join("нет папки")), Vec::new());
        fs::remove_dir_all(&game).unwrap();
    }

    #[test]
    fn recent_worlds_are_sorted_open_first_then_by_time_with_icons() {
        let game = temp_dir("recent");
        let other = temp_dir("recent-other");
        let saves = game.join("saves");
        let old = make_world(&saves, "Old");
        let new = make_world(&saves, "New");
        let open = make_world(&saves, "Open");
        let far = make_world(&other.join("saves"), "Other");
        fs::write(new.join("icon.png"), b"\x89PNG").unwrap();
        fs::write(far.join("icon.png"), vec![0u8; 70 * 1024]).unwrap();
        // Open time is the modification time of session.lock.
        let set_age = |dir: &Path, secs: u64| {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(dir.join(SESSION_LOCK))
                .unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(secs))
                .unwrap();
        };
        set_age(&old, 1000);
        set_age(&new, 100);
        set_age(&open, 5000);
        set_age(&far, 500);
        let _held = hold(&open);

        // A duplicate game folder does not double the worlds.
        let dirs = [game.clone(), other.clone(), game.clone()];
        let list = recent_in(&dirs, 10);
        let names: Vec<_> = list.iter().map(|w| w.folder.as_str()).collect();
        assert_eq!(names, ["Open", "New", "Other", "Old"]);
        assert!(list[0].open && !list[1].open);
        assert_eq!(list[1].icon.as_deref(), Some(&b"\x89PNG"[..]));
        assert_eq!(list[0].icon, None, "no icon");
        assert_eq!(list[2].icon, None, "icon larger than 64 KB");
        assert!(list[1].opened_ms > list[2].opened_ms);
        assert_eq!(recent_in(&dirs, 2).len(), 2);
        assert!(recent_in(&[game.join("нет")], 5).is_empty());
        drop(_held);
        fs::remove_dir_all(&game).unwrap();
        fs::remove_dir_all(&other).unwrap();
    }

    #[test]
    fn several_open_worlds_are_resolved_by_motd_then_by_time() {
        let now = SystemTime::now();
        let world = |name: &str, open: bool, age_s: u64| World {
            dir: PathBuf::from(name),
            name: name.into(),
            open,
            opened: Some(now - Duration::from_secs(age_s)),
        };
        let worlds = vec![
            world("Survival", true, 300),
            world("Creative", true, 10),
            world("Newest but closed", false, 1),
        ];
        // The folder name is in the LAN world's name: take it, even if another was opened earlier.
        let by_name = choose(worlds.clone(), Some("Alex - survival"));
        assert_eq!(by_name.unwrap().name, "Survival");
        // The name did not help: the last opened.
        assert_eq!(
            choose(worlds.clone(), Some("Мир Phase")).unwrap().name,
            "Creative"
        );
        assert_eq!(choose(worlds, None).unwrap().name, "Creative");
    }

    #[test]
    fn dedicated_server_world_comes_from_server_properties() {
        let server = temp_dir("server");
        assert_eq!(server_world(&server), None, "no server.properties");
        make_world(&server, "world");
        make_world(&server, "lobby");
        fs::write(
            server.join("server.properties"),
            "motd=Hi\nmax-players=20\n",
        )
        .unwrap();
        assert_eq!(server_world(&server).unwrap().name, "world");
        fs::write(
            server.join("server.properties"),
            "#Minecraft server properties\nlevel-name = lobby\r\nlevel-seed=1\n",
        )
        .unwrap();
        assert_eq!(server_world(&server).unwrap().name, "lobby");
        for outside in ["../other", "C:\\Windows", "a/b"] {
            fs::write(
                server.join("server.properties"),
                format!("level-name={outside}\n"),
            )
            .unwrap();
            assert_eq!(server_world(&server), None, "{outside}");
        }
        fs::remove_dir_all(&server).unwrap();
    }

    #[test]
    fn companion_counts_only_when_confirmed_in_this_world_session() {
        let game = temp_dir("companion");
        let dir = make_world(&game.join("saves"), "World");
        let alive = dir.join(COMPANION_FOLDER).join(COMPANION_ALIVE_FILE);
        let _held = hold(&dir);
        assert!(!World::at(&dir).unwrap().companion_alive(), "no file");

        fs::create_dir_all(alive.parent().unwrap()).unwrap();
        fs::write(&alive, b"ok").unwrap();
        assert!(World::at(&dir).unwrap().companion_alive());

        // The confirmation is left from a previous world open (the mod has been removed since).
        let mut world = World::at(&dir).unwrap();
        world.opened = Some(SystemTime::now() + Duration::from_secs(60));
        assert!(!world.companion_alive());
        // The world is closed: the mod is not working.
        let mut world = World::at(&dir).unwrap();
        world.open = false;
        assert!(!world.companion_alive());
        drop(_held);
        fs::remove_dir_all(&game).unwrap();
    }

    /// NBT string: `u16` length and MUTF-8.
    fn nbt_str(out: &mut Vec<u8>, text: &str) {
        let bytes = mutf8(text);
        out.extend((bytes.len() as u16).to_be_bytes());
        out.extend(bytes);
    }

    /// MUTF-8 as in Minecraft: `\0` → `C0 80`, characters outside the BMP as surrogate pairs of 3 bytes each.
    fn mutf8(text: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in text.encode_utf16() {
            match unit {
                0x0001..=0x007F => out.push(unit as u8),
                0x0000..=0x07FF => {
                    out.push(0xC0 | (unit >> 6) as u8);
                    out.push(0x80 | (unit & 0x3F) as u8);
                }
                _ => {
                    out.push(0xE0 | (unit >> 12) as u8);
                    out.push(0x80 | ((unit >> 6) & 0x3F) as u8);
                    out.push(0x80 | (unit & 0x3F) as u8);
                }
            }
        }
        out
    }

    /// Uncompressed `level.dat` NBT: root → `Data` → `LevelName`. Other tags precede `LevelName`
    /// (a number, a list of compounds, a byte array) to check that they are skipped.
    fn nbt_bytes(level_name: Option<&str>) -> Vec<u8> {
        let mut nbt = vec![TAG_COMPOUND];
        nbt_str(&mut nbt, "");
        nbt.push(TAG_COMPOUND);
        nbt_str(&mut nbt, "Data");
        nbt.push(4); // Long
        nbt_str(&mut nbt, "RandomSeed");
        nbt.extend(42i64.to_be_bytes());
        nbt.push(9); // List of compound
        nbt_str(&mut nbt, "Spawn");
        nbt.push(TAG_COMPOUND);
        nbt.extend(2i32.to_be_bytes());
        for _ in 0..2 {
            nbt.push(3); // Int
            nbt_str(&mut nbt, "x");
            nbt.extend(5i32.to_be_bytes());
            nbt.push(TAG_END);
        }
        nbt.push(7); // ByteArray
        nbt_str(&mut nbt, "Bytes");
        nbt.extend(3i32.to_be_bytes());
        nbt.extend([1, 2, 3]);
        if let Some(name) = level_name {
            nbt.push(TAG_STRING);
            nbt_str(&mut nbt, "LevelName");
            nbt_str(&mut nbt, name);
        }
        nbt.push(TAG_END); // end of Data
        nbt.push(TAG_END); // end of root
        nbt
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn world_name_is_level_name_from_gzip_nbt() {
        let game = temp_dir("levelname");
        let dir = make_world(&game.join("saves"), "Folder");
        fs::write(dir.join("level.dat"), gzip(&nbt_bytes(Some("Мой мир 🌍")))).unwrap();
        assert_eq!(level_name(&dir).as_deref(), Some("Мой мир 🌍"));
        assert_eq!(world_title(&dir, "Folder"), "Мой мир 🌍");
        let list = recent_in(std::slice::from_ref(&game), 5);
        assert_eq!(list[0].name, "Мой мир 🌍");
        assert_eq!(list[0].folder, "Folder");
        fs::remove_dir_all(&game).unwrap();
    }

    #[test]
    fn broken_level_dat_falls_back_to_folder_name() {
        let dir = temp_dir("broken").join("Папка");
        fs::create_dir_all(&dir).unwrap();
        let valid = nbt_bytes(Some("Имя"));
        let mut deep = vec![TAG_COMPOUND];
        nbt_str(&mut deep, "");
        for _ in 0..1000 {
            deep.push(TAG_COMPOUND);
            nbt_str(&mut deep, "a");
        }
        deep.extend(vec![TAG_END; 1001]);
        let mut huge = valid.clone();
        huge.extend(vec![0u8; MAX_LEVEL_DAT_BYTES as usize]);
        let mut without_data = vec![TAG_COMPOUND];
        nbt_str(&mut without_data, "");
        nbt_str(&mut without_data, "x");
        without_data.extend([TAG_STRING]);
        nbt_str(&mut without_data, "Other");
        nbt_str(&mut without_data, "y");
        without_data.push(TAG_END);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("not gzip", b"nbt".to_vec()),
            ("gzip but not NBT", gzip(&[0xFF; 100])),
            ("truncated NBT", gzip(&valid[..valid.len() / 2])),
            ("no Data", gzip(&without_data)),
            ("no LevelName", gzip(&nbt_bytes(None))),
            ("deep nesting", gzip(&deep)),
            ("over the limit", gzip(&huge)),
        ];
        for (case, bytes) in &cases {
            fs::write(dir.join("level.dat"), bytes).unwrap();
            assert_eq!(level_name(&dir), None, "{case}");
            assert_eq!(world_title(&dir, "Папка"), "Папка", "{case}");
        }
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn empty_or_color_only_level_name_falls_back_and_codes_are_stripped() {
        let dir = temp_dir("names").join("Folder");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("level.dat"), gzip(&nbt_bytes(Some("")))).unwrap();
        assert_eq!(world_title(&dir, "Folder"), "Folder");
        fs::write(dir.join("level.dat"), gzip(&nbt_bytes(Some("§a§l")))).unwrap();
        assert_eq!(world_title(&dir, "Folder"), "Folder");
        fs::write(
            dir.join("level.dat"),
            gzip(&nbt_bytes(Some("§aМой §lмир "))),
        )
        .unwrap();
        assert_eq!(world_title(&dir, "Folder"), "Мой мир");
        assert_eq!(strip_color_codes("§cКрасный"), "Красный");
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
