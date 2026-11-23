//! The `mods` folder: filtering and hashing of `.jar` files, the hash cache, protection against leaving
//! the folder, backup of extra mods and their return, folder selection.

use std::{
    fs,
    path::{Path, PathBuf},
};

use phase_minecraft::mods::{
    self, BACKUP_DIR, DirSource, HashCache, LEGACY_BACKUP_DIR, StashReport, validate_file_name,
};

/// A separate temporary folder per test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phase-mods-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn names(dir: &Path) -> Vec<String> {
    mods::list_jars(dir)
        .unwrap()
        .into_iter()
        .map(|jar| jar.file_name)
        .collect()
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn only_jars_in_the_root_are_listed_and_hashed() {
    let dir = temp_dir("scan");
    fs::write(dir.join("sodium.jar"), b"abc").unwrap();
    fs::write(dir.join("Iris.JAR"), b"iris").unwrap();
    fs::write(dir.join("readme.txt"), b"text").unwrap();
    fs::write(dir.join("half.jar.part"), b"part").unwrap();
    fs::write(dir.join("old.jar.disabled"), b"off").unwrap();
    fs::write(dir.join(".jar"), b"no name").unwrap();
    // The game does not read subfolders; a folder named like a mod is not a mod.
    fs::create_dir_all(dir.join("nested")).unwrap();
    fs::write(dir.join("nested").join("inner.jar"), b"inner").unwrap();
    fs::create_dir_all(dir.join("folder.jar")).unwrap();
    fs::create_dir_all(dir.join(BACKUP_DIR)).unwrap();
    fs::write(dir.join(BACKUP_DIR).join("stashed.jar"), b"stashed").unwrap();

    let mut cache = HashCache::in_memory();
    let found = mods::scan(&dir, &mut cache).unwrap();
    assert_eq!(
        found
            .iter()
            .map(|m| m.file_name.as_str())
            .collect::<Vec<_>>(),
        ["Iris.JAR", "sodium.jar"],
        "alphabetical, case-insensitive"
    );
    // Known hashes of the string "abc".
    let sodium = &found[1];
    assert_eq!(sodium.size, 3);
    assert_eq!(sodium.sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(
        sodium.blake3,
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
    assert_eq!(found[0].sha1.len(), 40);
    assert_eq!(found[0].blake3.len(), 64);
    assert_eq!(mods::hash_bytes(b"abc").sha1, sodium.sha1);

    // No folder means no mods; this is not an error.
    assert!(
        mods::scan(&dir.join("missing"), &mut cache)
            .unwrap()
            .is_empty()
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn hashes_are_cached_by_name_size_and_mtime() {
    let dir = temp_dir("cache");
    let cache_file = dir.join("cache").join("hashes.json");
    let mods_dir = dir.join("mods");
    fs::create_dir_all(&mods_dir).unwrap();
    fs::write(mods_dir.join("a.jar"), b"first").unwrap();
    fs::write(mods_dir.join("b.jar"), b"second").unwrap();

    let mut cache = HashCache::load(&cache_file);
    let first = mods::scan(&mods_dir, &mut cache).unwrap();
    assert_eq!(cache.computed(), 2);
    assert_eq!(mods::scan(&mods_dir, &mut cache).unwrap(), first);
    assert_eq!(
        cache.computed(),
        2,
        "the second scan does not read the files"
    );
    cache.save().unwrap();

    // The cache survives a program restart.
    let mut reloaded = HashCache::load(&cache_file);
    assert_eq!(mods::scan(&mods_dir, &mut reloaded).unwrap(), first);
    assert_eq!(reloaded.computed(), 0);

    // The file changed (different size): the hash is recomputed, and only for it.
    fs::write(mods_dir.join("a.jar"), b"first, but longer").unwrap();
    let changed = mods::scan(&mods_dir, &mut reloaded).unwrap();
    assert_eq!(reloaded.computed(), 1);
    assert_ne!(changed[0].blake3, first[0].blake3);
    assert_eq!(changed[1], first[1]);

    // A damaged cache file is just an empty cache.
    fs::write(&cache_file, b"{ not json").unwrap();
    let mut broken = HashCache::load(&cache_file);
    assert_eq!(mods::scan(&mods_dir, &mut broken).unwrap(), changed);
    assert_eq!(broken.computed(), 2);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn file_names_that_could_leave_the_mods_folder_are_rejected() {
    for good in [
        "sodium.jar",
        "Fabric API 0.92.2+1.20.1.jar",
        "мод [1.20].JAR",
        "a.b.c.jar",
    ] {
        assert!(validate_file_name(good).is_ok(), "{good}");
    }
    for bad in [
        "",
        ".jar",
        "../evil.jar",
        "..\\evil.jar",
        "a/../../evil.jar",
        "sub/evil.jar",
        "sub\\evil.jar",
        "/etc/evil.jar",
        "C:\\Windows\\evil.jar",
        "C:evil.jar",
        "\\\\server\\share\\evil.jar",
        "evil.jar:stream.jar",
        "evil..jar",
        "..jar",
        ".hidden.jar",
        "evil\0.jar",
        "evil.jar\0",
        "evil\n.jar",
        " evil.jar",
        "evil.jar ",
        "evil.jar.exe",
        "evil.exe",
        "evil.jar.part",
        "evil",
        "evil.jar.",
        "what?.jar",
        "a*b.jar",
        "a<b>.jar",
        "a|b.jar",
        "\"quoted\".jar",
        "CON.jar",
        "nul.jar",
        "com1.x.jar",
    ] {
        assert!(validate_file_name(bad).is_err(), "{bad:?} must be rejected");
    }
    assert!(validate_file_name(&format!("{}.jar", "x".repeat(196))).is_ok());
    assert!(validate_file_name(&format!("{}.jar", "x".repeat(197))).is_err());
}

#[test]
fn stash_and_install_refuse_path_traversal_without_touching_files() {
    let root = temp_dir("traversal");
    let mods_dir = root.join("mods");
    fs::create_dir_all(&mods_dir).unwrap();
    fs::write(root.join("evil.jar"), b"outside").unwrap();
    fs::write(mods_dir.join("good.jar"), b"good").unwrap();

    // One bad name in the list: no file is touched, not even a good one.
    let err = mods::stash(&mods_dir, &strings(&["good.jar", "../evil.jar"])).unwrap_err();
    assert!(format!("{err:#}").contains("evil.jar"), "{err:#}");
    assert!(mods_dir.join("good.jar").is_file());
    assert_eq!(fs::read(root.join("evil.jar")).unwrap(), b"outside");
    assert!(!mods_dir.join(BACKUP_DIR).exists());

    assert!(mods::part_path(&mods_dir, "../evil.jar").is_err());
    assert!(mods::part_path(&mods_dir, "C:\\evil.jar").is_err());
    assert!(mods::install(&mods_dir, "..\\evil.jar").is_err());
    assert_eq!(
        mods::part_path(&mods_dir, "new.jar").unwrap(),
        mods_dir.join("new.jar.part")
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn old_backup_folder_is_moved_or_read_alongside() {
    // A backup made by LanBridge itself moves into the new folder and is restored.
    let dir = temp_dir("legacy-backup");
    fs::create_dir_all(dir.join(LEGACY_BACKUP_DIR)).unwrap();
    fs::write(dir.join(LEGACY_BACKUP_DIR).join("mine.jar"), b"mine").unwrap();
    fs::write(dir.join("host.jar"), b"host").unwrap();
    fs::write(
        dir.join(LEGACY_BACKUP_DIR).join("installed.json"),
        br#"{"installed":[{"file_name":"host.jar","size":4}]}"#,
    )
    .unwrap();
    let info = mods::backup_info(&dir);
    assert_eq!((info.files, info.installed), (1, 1));
    assert!(
        !dir.join(LEGACY_BACKUP_DIR).exists(),
        "the old folder was renamed"
    );
    assert_eq!(names(&dir.join(BACKUP_DIR)), ["mine.jar"]);

    // Both folders at once (the old program version made a backup again): "Restore my
    // mods" takes from both, and the list of installed files is read from both too.
    fs::create_dir_all(dir.join(LEGACY_BACKUP_DIR)).unwrap();
    fs::write(dir.join(LEGACY_BACKUP_DIR).join("old.jar"), b"old").unwrap();
    assert_eq!(mods::backup_info(&dir).files, 2);
    let restored = mods::restore(&dir).unwrap();
    assert_eq!(restored.restored, ["mine.jar", "old.jar"]);
    assert_eq!(restored.removed, ["host.jar"]);
    assert_eq!(names(&dir), ["mine.jar", "old.jar"]);
    assert!(!dir.join(BACKUP_DIR).exists() && !dir.join(LEGACY_BACKUP_DIR).exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn extra_mods_go_to_backup_and_come_back() {
    let dir = temp_dir("backup");
    fs::write(dir.join("mine-a.jar"), b"my mod a").unwrap();
    fs::write(dir.join("mine-b.jar"), b"my mod b").unwrap();
    fs::write(dir.join("shared.jar"), b"same as host").unwrap();
    assert!(!mods::backup_info(&dir).can_restore());

    // Sync: extra ones go to the backup, a missing host mod is downloaded and installed.
    let report = mods::stash(&dir, &strings(&["mine-a.jar", "mine-b.jar", "gone.jar"])).unwrap();
    assert_eq!(
        report,
        StashReport {
            moved: strings(&["mine-a.jar", "mine-b.jar"]),
            ..StashReport::default()
        }
    );
    fs::write(mods::part_path(&dir, "host-mod.jar").unwrap(), b"host mod").unwrap();
    assert_eq!(
        names(&dir),
        ["shared.jar"],
        "a partly downloaded file is not a mod yet"
    );
    mods::install(&dir, "host-mod.jar").unwrap();

    assert_eq!(names(&dir), ["host-mod.jar", "shared.jar"]);
    assert_eq!(names(&dir.join(BACKUP_DIR)), ["mine-a.jar", "mine-b.jar"]);
    assert_eq!(
        fs::read(dir.join(BACKUP_DIR).join("mine-a.jar")).unwrap(),
        b"my mod a"
    );
    let info = mods::backup_info(&dir);
    assert_eq!((info.files, info.installed), (2, 1));
    assert!(info.can_restore());

    // "Restore my mods": the host mod is removed, the user's own are back, the shared one stays.
    let restored = mods::restore(&dir).unwrap();
    assert_eq!(restored.restored, ["mine-a.jar", "mine-b.jar"]);
    assert_eq!(restored.removed, ["host-mod.jar"]);
    assert!(restored.conflicts.is_empty() && restored.failed.is_empty());
    assert_eq!(names(&dir), ["mine-a.jar", "mine-b.jar", "shared.jar"]);
    assert_eq!(fs::read(dir.join("mine-b.jar")).unwrap(), b"my mod b");
    assert!(
        !dir.join(BACKUP_DIR).exists(),
        "an empty backup folder is removed"
    );
    assert!(!mods::backup_info(&dir).can_restore());

    // Nothing to restore: nothing happens.
    assert_eq!(mods::restore(&dir).unwrap(), mods::RestoreReport::default());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn second_sync_removes_host_mods_but_never_loses_own_mods() {
    let dir = temp_dir("second");
    fs::write(dir.join("mine.jar"), b"mine").unwrap();

    // First host: my mod goes to the backup, pack.jar and mine.jar (another version) are installed.
    mods::stash(&dir, &strings(&["mine.jar"])).unwrap();
    for (name, data) in [
        ("pack.jar", &b"host one pack"[..]),
        ("mine.jar", b"host one version"),
    ] {
        fs::write(mods::part_path(&dir, name).unwrap(), data).unwrap();
        mods::install(&dir, name).unwrap();
    }

    // Second host: the first one's mods are extra. The program installed them, so they are deleted and do not
    // overwrite my mod in the backup.
    let report = mods::stash(&dir, &strings(&["mine.jar", "pack.jar"])).unwrap();
    assert_eq!(report.removed, ["mine.jar", "pack.jar"]);
    assert!(report.moved.is_empty());
    assert_eq!(names(&dir.join(BACKUP_DIR)), ["mine.jar"]);
    assert_eq!(
        fs::read(dir.join(BACKUP_DIR).join("mine.jar")).unwrap(),
        b"mine"
    );

    // The user put a new mine.jar themselves: it goes to the backup too, under a different name.
    fs::write(dir.join("mine.jar"), b"mine, newer").unwrap();
    let report = mods::stash(&dir, &strings(&["mine.jar"])).unwrap();
    assert_eq!(report.moved, ["mine.jar"]);
    assert_eq!(names(&dir.join(BACKUP_DIR)), ["mine (2).jar", "mine.jar"]);

    // Restore does not overwrite a file that appeared in the mods folder later.
    fs::write(dir.join("mine.jar"), b"placed by hand").unwrap();
    let restored = mods::restore(&dir).unwrap();
    assert_eq!(restored.restored, ["mine (2).jar"]);
    assert_eq!(restored.conflicts, ["mine.jar"]);
    assert_eq!(fs::read(dir.join("mine.jar")).unwrap(), b"placed by hand");
    assert_eq!(
        fs::read(dir.join(BACKUP_DIR).join("mine.jar")).unwrap(),
        b"mine"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn restore_keeps_a_host_mod_the_user_replaced() {
    let dir = temp_dir("replaced");
    fs::write(mods::part_path(&dir, "pack.jar").unwrap(), b"from host").unwrap();
    mods::install(&dir, "pack.jar").unwrap();
    // The user updated the mod themselves: it is their file now.
    fs::write(dir.join("pack.jar"), b"updated by the user").unwrap();
    let restored = mods::restore(&dir).unwrap();
    assert!(restored.removed.is_empty());
    assert_eq!(
        fs::read(dir.join("pack.jar")).unwrap(),
        b"updated by the user"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn game_dir_is_read_from_the_command_line() {
    let line = r#""C:\Program Files\Java\bin\javaw.exe" -Xmx4G -cp libs net.minecraft.client.main.Main --username Steve --version 1.20.1 --gameDir "C:\Users\Алекс\AppData\Roaming\.minecraft" --assetsDir assets --accessToken secret"#;
    assert_eq!(
        mods::game_dir_from_command_line(line),
        Some(PathBuf::from(r"C:\Users\Алекс\AppData\Roaming\.minecraft"))
    );
    assert_eq!(
        mods::game_dir_from_command_line(
            r"javaw --gameDir D:\Prism\instances\pack\.minecraft --width 854"
        ),
        Some(PathBuf::from(r"D:\Prism\instances\pack\.minecraft"))
    );
    // A relative path does not count as a game folder; another Java program is rejected.
    assert_eq!(mods::game_dir_from_command_line("javaw --gameDir mc"), None);
    assert_eq!(mods::game_dir_from_command_line("java -jar idea.jar"), None);
}

#[test]
fn mods_folder_order_running_game_then_chosen_then_default() {
    let root = temp_dir("choose");
    let (game, chosen, default) = (root.join("game"), root.join("chosen"), root.join("default"));
    fs::create_dir_all(&game).unwrap();
    fs::create_dir_all(&chosen).unwrap();

    let pick = |running: &[PathBuf], chosen: Option<&Path>| {
        let found = mods::choose(running, chosen, Some(&default)).unwrap();
        (found.path, found.source, found.exists)
    };
    // The running game takes priority over the chosen folder.
    assert_eq!(
        pick(std::slice::from_ref(&game), Some(&chosen)),
        (game.clone(), DirSource::RunningGame, true)
    );
    // The running game has no mods folder: the chosen one is used.
    let vanilla = root.join("vanilla");
    assert_eq!(
        pick(std::slice::from_ref(&vanilla), Some(&chosen)),
        (chosen.clone(), DirSource::Chosen, true)
    );
    // Of two running games, the one that has mods is taken.
    assert_eq!(
        pick(&[vanilla.clone(), game.clone()], None),
        (game.clone(), DirSource::RunningGame, true)
    );
    // Game not running: the chosen folder, and without it the standard one.
    assert_eq!(
        pick(&[], Some(&chosen)),
        (chosen.clone(), DirSource::Chosen, true)
    );
    assert_eq!(
        pick(&[], None),
        (default.clone(), DirSource::Default, false)
    );
    // Game running without mods and nothing chosen: mods are needed by this game.
    assert_eq!(
        pick(std::slice::from_ref(&vanilla), None),
        (vanilla.clone(), DirSource::RunningGame, false)
    );
    assert_eq!(mods::choose(&[], None, None), None);
    fs::remove_dir_all(&root).unwrap();
}
