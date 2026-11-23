//! End-to-end tests of mod sync at the session level: the host distributes the `mods` folder, the guest
//! checks their own, moves the extras to the backup, downloads the missing ones (Modrinth → host) and
//! can return everything as it was. The host and guest are on one PC (`127.0.0.1`), the mods folders are
//! temporary, a running game is not searched for.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use phase_core::{
    EventBus, GamePort, GuestMods, HostMods, HostOptions, HostVoice, JoinOptions, ModCommand,
    ModSyncState, ModSyncStatus, ModrinthSource, ModsFolder, NetOptions, Phase, Session,
    SessionStatus,
};
use phase_minecraft::{
    lan::AnnounceMode,
    mods::{self as mc_mods, BACKUP_DIR},
};
use phase_tunnel::{EndpointTicket, endpoint::NetworkMode, identity};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const WAIT: Duration = Duration::from_secs(30);

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "phase-core-mods-{name}-{}-{}",
        std::process::id(),
        phase_tunnel::metrics::unix_now()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A "mod" content: reproducible bytes of a given length.
fn content(len: usize, seed: u8) -> Vec<u8> {
    let mut state = u32::from(seed) + 7;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

fn write_mods(dir: &Path, files: &[(&str, &[u8])]) {
    std::fs::create_dir_all(dir).unwrap();
    for (name, data) in files {
        std::fs::write(dir.join(name), data).unwrap();
    }
}

/// `.jar` names in the folder (alphabetical): what the game will see.
fn jars(dir: &Path) -> Vec<String> {
    mc_mods::list_jars(dir)
        .unwrap()
        .into_iter()
        .map(|jar| jar.file_name)
        .collect()
}

/// All files of the folder except subfolders: no partly downloaded `.part` must remain.
fn leftovers(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.to_lowercase().ends_with(".jar"))
        .collect()
}

fn folder(dir: &Path) -> ModsFolder {
    ModsFolder {
        chosen: Some(dir.to_path_buf()),
        detect_running_game: false,
    }
}

fn host_mods(dir: &Path, excluded: &[&str]) -> HostMods {
    HostMods {
        share: true,
        folder: folder(dir),
        excluded: excluded.iter().map(|name| name.to_string()).collect(),
    }
}

/// The "game": a TCP echo on 127.0.0.1.
async fn fake_game() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = tcp.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

struct Pair {
    host: Session,
    guest: Session,
    _bus: EventBus,
}

/// A host with mod distribution and a guest allowed in advance.
async fn start_pair(dir: &Path, host: HostMods, guest: GuestMods) -> Pair {
    let bus = EventBus::new();
    let loopback = NetOptions {
        mode: NetworkMode::LoopbackOnly,
        ..NetOptions::default()
    };
    let guest_id = identity::load_or_create_secret_key(&dir.join("guest"))
        .unwrap()
        .public();

    let mut opts = HostOptions::new("host");
    opts.profile_dir = Some(dir.join("host"));
    opts.nickname = Some("HostNick".into());
    opts.game_port = Some(GamePort::Fixed(fake_game().await));
    opts.voice = HostVoice::Off;
    opts.allow = vec![guest_id];
    opts.net = loopback.clone();
    opts.mods = host;
    opts.mod_hash_cache = Some(dir.join("host-hashes.json"));
    let host = Session::host(opts, bus.clone());
    let ready = wait_status(&host, |s| s.phase == Phase::Running).await;
    let ticket: EndpointTicket = ready.host.unwrap().ticket.parse().unwrap();

    let mut opts = JoinOptions::new("guest", ticket);
    opts.profile_dir = Some(dir.join("guest"));
    opts.nickname = Some("GuestNick".into());
    opts.announce = AnnounceMode::Off;
    opts.voice = false;
    opts.net = loopback;
    opts.mods = guest;
    let guest = Session::join(opts, bus.clone());
    wait_status(&guest, |s| s.guest.is_some()).await;
    Pair {
        host,
        guest,
        _bus: bus,
    }
}

async fn wait_status(session: &Session, what: impl Fn(&SessionStatus) -> bool) -> SessionStatus {
    let mut rx = session.watch_status();
    let status = tokio::time::timeout(WAIT, rx.wait_for(|s| what(s) || s.phase.is_finished()))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the session did not reach the needed state: {:?}",
                session.status()
            )
        })
        .unwrap()
        .clone();
    assert!(what(&status), "the session finished earlier: {status:?}");
    status
}

/// Waits for the needed sync stage at the guest.
async fn wait_mods(guest: &Session, state: ModSyncState) -> ModSyncStatus {
    let reached = |s: &SessionStatus| s.guest.as_ref().is_some_and(|g| g.mods.state == state);
    wait_status(guest, reached).await.guest.unwrap().mods
}

fn sha1(data: &[u8]) -> String {
    mc_mods::hash_bytes(data).sha1
}

/// A fake Modrinth on 127.0.0.1: for a SHA-1 it answers with a version description and serves the file
/// from its own address. `files`: SHA-1 of a mod → what the server gives instead of the file.
/// Returns the settings for the guest and a counter of "is there such a mod" questions.
async fn fake_modrinth(files: HashMap<String, Vec<u8>>) -> (ModrinthSource, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let lookups = Arc::new(AtomicUsize::new(0));
    let files = Arc::new(files);
    tokio::spawn({
        let (base, lookups) = (base.clone(), lookups.clone());
        async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let (base, lookups, files) = (base.clone(), lookups.clone(), files.clone());
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 2048];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tcp.read(&mut buf).await {
                            Ok(n) if n > 0 => request.extend_from_slice(&buf[..n]),
                            _ => return,
                        }
                    }
                    let request = String::from_utf8_lossy(&request).into_owned();
                    let path = request.split_whitespace().nth(1).unwrap_or_default();
                    let (status, body) = if let Some(rest) = path.strip_prefix("/v2/version_file/")
                    {
                        lookups.fetch_add(1, Ordering::SeqCst);
                        let hash = rest.split('?').next().unwrap_or_default();
                        match files.get(hash) {
                            Some(data) if rest.ends_with("?algorithm=sha1") => {
                                let version = serde_json::json!({
                                    "project_id": "test",
                                    "files": [{
                                        "hashes": { "sha1": hash },
                                        "url": format!("{base}/cdn/{hash}.jar"),
                                        "filename": "mod.jar",
                                        "primary": true,
                                        "size": data.len(),
                                    }],
                                });
                                ("200 OK", version.to_string().into_bytes())
                            }
                            _ => ("404 Not Found", Vec::new()),
                        }
                    } else if let Some(hash) = path
                        .strip_prefix("/cdn/")
                        .and_then(|name| name.strip_suffix(".jar"))
                        && let Some(data) = files.get(hash)
                    {
                        ("200 OK", data.clone())
                    } else {
                        ("404 Not Found", Vec::new())
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = tcp.write_all(head.as_bytes()).await;
                    let _ = tcp.write_all(&body).await;
                    let _ = tcp.shutdown().await;
                });
            }
        }
    });
    let source = ModrinthSource {
        api: base.clone(),
        cdn_prefixes: vec![format!("{base}/cdn/")],
        lookup_timeout: Duration::from_secs(3),
    };
    (source, lookups)
}

#[tokio::test]
async fn guest_syncs_with_host_then_gets_own_mods_back() {
    let dir = temp_dir("p2p");
    let (host_dir, guest_dir) = (dir.join("host-mods"), dir.join("guest-mods"));
    let pack_a = content(300_000, 1);
    let pack_b = content(4000, 2);
    let old_a = content(2000, 3);
    write_mods(
        &host_dir,
        &[
            ("pack-a.jar", &pack_a),
            ("pack-b.jar", &pack_b),
            ("shared.jar", b"the same file"),
            ("shader.jar", b"client-only shader"),
            ("notes.txt", b"not a mod"),
        ],
    );
    write_mods(
        &guest_dir,
        &[
            ("shared.jar", b"the same file"),
            ("my-shader.jar", b"client-only shader"),
            ("mine.jar", b"my own mod"),
            ("pack-a.jar", &old_a),
        ],
    );

    // The host unticked the shaders. The guest: without Modrinth, sync by button.
    let pair = start_pair(
        &dir,
        host_mods(&host_dir, &["Shader.jar"]),
        GuestMods {
            folder: folder(&guest_dir),
            auto: false,
            modrinth: None,
        },
    )
    .await;
    let shared = pair.host.status().host.unwrap().mods;
    assert!(shared.sharing && shared.error.is_none(), "{shared:?}");
    assert_eq!((shared.files, shared.excluded), (3, 1));
    assert_eq!(shared.total_size, (pack_a.len() + pack_b.len() + 13) as u64);
    assert_eq!(shared.dir.as_deref(), Some(host_dir.to_str().unwrap()));

    // Check: two are missing, two are extra (their own mod and an old version of pack-a). Nothing
    // is touched until the guest presses the button.
    let found = wait_mods(&pair.guest, ModSyncState::NeedsSync).await;
    assert!(found.host_shares && !found.auto);
    assert_eq!((found.required, found.missing, found.extra), (3, 2, 2));
    assert_eq!(found.missing_bytes, (pack_a.len() + pack_b.len()) as u64);
    assert_eq!(found.missing_names, ["pack-a.jar", "pack-b.jar"]);
    assert_eq!(found.extra_names, ["mine.jar", "pack-a.jar"]);
    assert!(!found.can_restore);
    assert_eq!(std::fs::read(guest_dir.join("pack-a.jar")).unwrap(), old_a);

    assert!(pair.guest.mod_command(ModCommand::Sync));
    let done = wait_mods(&pair.guest, ModSyncState::Done).await;
    assert_eq!((done.missing, done.extra), (0, 0));
    assert_eq!(
        (done.from_modrinth, done.from_host, done.backed_up),
        (0, 2, 2)
    );
    assert_eq!(done.backup_files, 2);
    assert!(done.can_restore && done.error.is_none() && !done.needs_mc_restart);
    // The folder has the host's mods and own shaders (the same as the host's); the extra goes to the backup.
    assert_eq!(
        jars(&guest_dir),
        ["my-shader.jar", "pack-a.jar", "pack-b.jar", "shared.jar"]
    );
    assert_eq!(std::fs::read(guest_dir.join("pack-a.jar")).unwrap(), pack_a);
    assert_eq!(std::fs::read(guest_dir.join("pack-b.jar")).unwrap(), pack_b);
    assert_eq!(
        jars(&guest_dir.join(BACKUP_DIR)),
        ["mine.jar", "pack-a.jar"]
    );
    assert_eq!(
        std::fs::read(guest_dir.join(BACKUP_DIR).join("pack-a.jar")).unwrap(),
        old_a
    );
    assert!(
        leftovers(&guest_dir).is_empty(),
        "{:?}",
        leftovers(&guest_dir)
    );

    // The host sees how much it served itself.
    let sent = wait_status(&pair.host, |s| {
        s.host.as_ref().is_some_and(|h| h.mods.sent_files == 2)
    })
    .await
    .host
    .unwrap()
    .mods;
    assert_eq!(sent.sent_bytes, (pack_a.len() + pack_b.len()) as u64);

    // "Restore my mods": everything as before the sync.
    assert!(pair.guest.mod_command(ModCommand::Restore));
    let restored = wait_mods(&pair.guest, ModSyncState::Skipped).await;
    assert_eq!((restored.missing, restored.extra), (2, 2));
    assert!(!restored.can_restore);
    assert_eq!(
        jars(&guest_dir),
        ["mine.jar", "my-shader.jar", "pack-a.jar", "shared.jar"]
    );
    assert_eq!(std::fs::read(guest_dir.join("pack-a.jar")).unwrap(), old_a);
    assert_eq!(
        std::fs::read(guest_dir.join("mine.jar")).unwrap(),
        b"my own mod"
    );
    assert!(!guest_dir.join(BACKUP_DIR).exists());

    // Host commands do not suit a guest, and vice versa.
    assert!(!pair.host.mod_command(ModCommand::Sync));
    assert!(!pair.guest.set_host_mods(HostMods::default()));
    pair.guest.shutdown().await.unwrap();
    pair.host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn auto_sync_takes_mods_from_modrinth_first_and_from_host_otherwise() {
    let dir = temp_dir("modrinth");
    let (host_dir, guest_dir) = (dir.join("host-mods"), dir.join("guest-mods"));
    let on_modrinth = content(150_000, 11);
    let swapped = content(9000, 12);
    let private = content(5000, 13);
    write_mods(
        &host_dir,
        &[
            ("on-modrinth.jar", &on_modrinth),
            ("swapped.jar", &swapped),
            ("private.jar", &private),
        ],
    );
    // Modrinth knows two mods, but instead of the second it gives another file of the same size.
    let (source, lookups) = fake_modrinth(HashMap::from([
        (sha1(&on_modrinth), on_modrinth.clone()),
        (sha1(&swapped), content(9000, 99)),
    ]))
    .await;

    // The guest does not have a mods folder yet: it will appear during sync.
    let pair = start_pair(
        &dir,
        host_mods(&host_dir, &[]),
        GuestMods {
            folder: folder(&guest_dir),
            auto: true,
            modrinth: Some(source),
        },
    )
    .await;
    let done = wait_mods(&pair.guest, ModSyncState::Done).await;
    assert!(done.auto);
    assert_eq!(
        (done.from_modrinth, done.from_host, done.backed_up),
        (1, 2, 0)
    );
    assert_eq!(
        lookups.load(Ordering::SeqCst),
        3,
        "every mod is looked up on Modrinth first"
    );
    assert_eq!(
        jars(&guest_dir),
        ["on-modrinth.jar", "private.jar", "swapped.jar"]
    );
    // The swapped file did not get into the folder: the mod was taken from the host and matches its mod.
    assert_eq!(
        std::fs::read(guest_dir.join("swapped.jar")).unwrap(),
        swapped
    );
    assert_eq!(
        std::fs::read(guest_dir.join("on-modrinth.jar")).unwrap(),
        on_modrinth
    );
    assert!(
        leftovers(&guest_dir).is_empty(),
        "{:?}",
        leftovers(&guest_dir)
    );

    // The host's channel is busy only with what Modrinth does not have.
    let sent = wait_status(&pair.host, |s| {
        s.host.as_ref().is_some_and(|h| h.mods.sent_files == 2)
    })
    .await;
    assert_eq!(
        sent.host.unwrap().mods.sent_bytes,
        (swapped.len() + private.len()) as u64
    );
    // All installed files can be removed with one button, although there were no own mods.
    assert!(done.can_restore);
    assert_eq!(done.backup_files, 0);
    pair.guest.shutdown().await.unwrap();
    pair.host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A custom pack mod: a disabled one (`.jar.disabled`) does not go to guests, after enabling it goes
/// like any other mod.
#[cfg(feature = "client")]
#[tokio::test]
async fn pack_mod_toggle_reaches_the_guest() {
    use phase_core::pack_mods;
    let dir = temp_dir("pack");
    let (host_dir, guest_dir) = (dir.join("host-mods"), dir.join("guest-mods"));
    write_mods(
        &host_dir,
        &[
            ("base.jar", b"base mod"),
            ("custom.jar", b"my own pack mod"),
        ],
    );
    write_mods(&guest_dir, &[]);
    assert!(pack_mods::set_enabled(&host_dir, "custom.jar", false).unwrap());
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let pair = start_pair(
        &dir,
        host_mods(&host_dir, &[]),
        GuestMods {
            folder: folder(&guest_dir),
            auto: false,
            modrinth: Some(ModrinthSource {
                api: format!("http://{dead_addr}"),
                cdn_prefixes: vec![format!("http://{dead_addr}/")],
                lookup_timeout: Duration::from_secs(3),
            }),
        },
    )
    .await;
    let found = wait_mods(&pair.guest, ModSyncState::NeedsSync).await;
    assert_eq!(found.missing_names, ["base.jar"]);

    // The custom mod was enabled: the host rebuilds the distribution, the guest gets it.
    assert!(pack_mods::set_enabled(&host_dir, "custom.jar", true).unwrap());
    assert!(pair.host.set_host_mods(host_mods(&host_dir, &[])));
    wait_status(&pair.host, |s| {
        s.host.as_ref().is_some_and(|h| h.mods.files == 2)
    })
    .await;
    pair.guest.mod_command(ModCommand::Recheck);
    // The previous check also says "sync needed": we wait for the new one specifically.
    let again = wait_status(&pair.guest, |s| {
        s.guest.as_ref().is_some_and(|g| g.mods.missing == 2)
    })
    .await
    .guest
    .unwrap()
    .mods;
    assert_eq!(again.missing_names, ["base.jar", "custom.jar"]);
    pair.guest.mod_command(ModCommand::Sync);
    wait_mods(&pair.guest, ModSyncState::Done).await;
    assert_eq!(jars(&guest_dir), ["base.jar", "custom.jar"]);
    assert_eq!(
        std::fs::read(guest_dir.join("custom.jar")).unwrap(),
        b"my own pack mod"
    );
    pair.guest.shutdown().await.unwrap();
    pair.host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn skip_recheck_and_host_changing_the_share_while_the_room_is_open() {
    let dir = temp_dir("skip");
    let (host_dir, guest_dir) = (dir.join("host-mods"), dir.join("guest-mods"));
    write_mods(&host_dir, &[("a.jar", b"mod a"), ("b.jar", b"mod b")]);
    write_mods(&guest_dir, &[("mine.jar", b"mine")]);
    // Modrinth is "down": an address nobody listens on.
    let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead_addr = dead.local_addr().unwrap();
    drop(dead);
    let pair = start_pair(
        &dir,
        host_mods(&host_dir, &[]),
        GuestMods {
            folder: folder(&guest_dir),
            auto: false,
            modrinth: Some(ModrinthSource {
                api: format!("http://{dead_addr}"),
                cdn_prefixes: vec![format!("http://{dead_addr}/")],
                lookup_timeout: Duration::from_secs(3),
            }),
        },
    )
    .await;

    // "Skip": joining without mods, the guest's folder is untouched.
    wait_mods(&pair.guest, ModSyncState::NeedsSync).await;
    pair.guest.mod_command(ModCommand::Skip);
    let skipped = wait_mods(&pair.guest, ModSyncState::Skipped).await;
    assert_eq!((skipped.missing, skipped.extra), (2, 1));
    assert_eq!(jars(&guest_dir), ["mine.jar"]);

    // The host turned distribution off without closing the room: no mods required anymore.
    assert!(pair.host.set_host_mods(HostMods::default()));
    wait_status(&pair.host, |s| {
        s.host.as_ref().is_some_and(|h| !h.mods.sharing)
    })
    .await;
    pair.guest.mod_command(ModCommand::Recheck);
    let off = wait_mods(&pair.guest, ModSyncState::NotRequired).await;
    assert_eq!((off.required, off.missing, off.extra), (0, 0, 0));
    assert!(off.host_shares, "the check can be repeated");

    // Enabled again, but without b.jar: the guest needs only a.jar.
    assert!(pair.host.set_host_mods(host_mods(&host_dir, &["b.jar"])));
    wait_status(&pair.host, |s| {
        s.host
            .as_ref()
            .is_some_and(|h| h.mods.files == 1 && h.mods.excluded == 1)
    })
    .await;
    pair.guest.mod_command(ModCommand::Recheck);
    let again = wait_mods(&pair.guest, ModSyncState::NeedsSync).await;
    assert_eq!((again.required, again.missing, again.extra), (1, 1, 1));

    // Modrinth is unavailable: everything is downloaded from the host.
    pair.guest.mod_command(ModCommand::Sync);
    let done = wait_mods(&pair.guest, ModSyncState::Done).await;
    assert_eq!(
        (done.from_modrinth, done.from_host, done.backed_up),
        (0, 1, 1)
    );
    assert_eq!(jars(&guest_dir), ["a.jar"]);
    assert_eq!(jars(&guest_dir.join(BACKUP_DIR)), ["mine.jar"]);
    pair.guest.shutdown().await.unwrap();
    pair.host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn host_without_sharing_requires_no_mods() {
    let dir = temp_dir("off");
    let guest_dir = dir.join("guest-mods");
    write_mods(&guest_dir, &[("mine.jar", b"mine")]);
    let pair = start_pair(
        &dir,
        HostMods::default(),
        GuestMods {
            folder: folder(&guest_dir),
            auto: true,
            modrinth: None,
        },
    )
    .await;
    let status = wait_status(&pair.guest, |s| {
        s.guest.as_ref().is_some_and(|g| g.mods.dir.is_some())
    })
    .await
    .guest
    .unwrap()
    .mods;
    assert_eq!(status.state, ModSyncState::NotRequired);
    assert_eq!((status.required, status.extra), (0, 0));
    assert_eq!(
        jars(&guest_dir),
        ["mine.jar"],
        "with no mods at the host the guest's folder is not touched"
    );
    let host = pair.host.status().host.unwrap().mods;
    assert!(!host.sharing && host.files == 0);
    pair.guest.shutdown().await.unwrap();
    pair.host.shutdown().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
