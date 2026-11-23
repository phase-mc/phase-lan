//! Mods through the tunnel: the guest gets the host's mod list and downloads files over separate
//! QUIC streams, with BLAKE3 checks and resume. Everything on 127.0.0.1.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use phase_tunnel::{
    EndpointAddr, SecretKey,
    access::{AccessHook, AccessList},
    endpoint::{self, EndpointOptions, NetworkMode},
    events::{EventSink, TunnelEvent},
    guest,
    host::{self, HostConfig},
    metrics::Telemetry,
    mods::{self, ModEntry, ModShare, SharedMod},
    wire::ModsOffer,
};

const WAIT: Duration = Duration::from_secs(20);

/// A separate temp folder per test.
fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phase-tunnel-mods-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// "Mod" contents: reproducible but non-repeating bytes.
fn content(len: usize, seed: u8) -> Vec<u8> {
    let mut state = u32::from(seed) + 1;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

/// Puts a mod into the host's folder and describes it for the share.
fn host_mod(dir: &Path, name: &str, data: &[u8]) -> SharedMod {
    let path = dir.join(name);
    std::fs::write(&path, data).unwrap();
    SharedMod {
        entry: ModEntry {
            file_name: name.into(),
            size: data.len() as u64,
            sha1: "a".repeat(40),
            blake3: blake3::hash(data).to_hex().to_string(),
        },
        path,
    }
}

struct Host {
    addr: EndpointAddr,
    share: ModShare,
    events: Arc<Mutex<Vec<TunnelEvent>>>,
}

impl Host {
    /// How many bytes the host sent for each file (`ModSent` events).
    fn sent(&self) -> Vec<(String, u64)> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                TunnelEvent::ModSent {
                    file_name, size, ..
                } => Some((file_name.clone(), *size)),
                _ => None,
            })
            .collect()
    }

    /// Waits until the host reports sending `count` files.
    async fn wait_sent(&self, count: usize) -> Vec<(String, u64)> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let sent = self.sent();
            if sent.len() >= count {
                return sent;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the host did not report: {sent:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// A host without a game, with a mod share; the guest `guest_key` is allowed in advance.
async fn start_host(share: ModShare, guest_key: &SecretKey) -> Host {
    let events: Arc<Mutex<Vec<TunnelEvent>>> = Arc::default();
    let sink = EventSink::new({
        let events = events.clone();
        move |event| events.lock().unwrap().push(event)
    });
    let access = AccessList::in_memory([guest_key.public()]);
    let ep = endpoint::bind(EndpointOptions {
        secret_key: SecretKey::generate(),
        accept_incoming: true,
        network: NetworkMode::LoopbackOnly,
        relay: Default::default(),
        access: Some(AccessHook::new(access.clone(), None)),
    })
    .await
    .unwrap();
    let addr = endpoint::shareable_addr(&ep, NetworkMode::LoopbackOnly, Duration::ZERO).await;
    let cfg = HostConfig {
        game_port: Default::default(),
        motd: "Мир с модами".into(),
        nickname: None,
        udp_ports: Vec::new(),
        game_version: None,
        check_voice_owner: true,
        status_every: Duration::from_secs(60),
        mods: share.clone(),
        room: Default::default(),
    };
    tokio::spawn(host::serve(ep, access, cfg, Telemetry::with_events(sink)));
    Host {
        addr,
        share,
        events,
    }
}

async fn guest_endpoint(key: SecretKey) -> iroh::Endpoint {
    endpoint::bind(EndpointOptions {
        secret_key: key,
        accept_incoming: false,
        network: NetworkMode::LoopbackOnly,
        relay: Default::default(),
        access: None,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn guest_downloads_mods_from_the_host_through_the_tunnel() {
    let dir = temp_dir("download");
    let (host_dir, guest_dir) = (dir.join("host"), dir.join("guest"));
    std::fs::create_dir_all(&host_dir).unwrap();
    std::fs::create_dir_all(&guest_dir).unwrap();
    // A big file goes in many chunks, a small one in one, an empty one with no data at all.
    let big = content(700_000, 1);
    let small = content(1234, 2);
    let share = ModShare::new();
    share
        .set(
            vec![
                host_mod(&host_dir, "big-pack.jar", &big),
                host_mod(&host_dir, "Маленький мод [1.20.1].jar", &small),
                host_mod(&host_dir, "empty.jar", b""),
            ],
            vec!["c".repeat(64)],
        )
        .unwrap();

    let key = SecretKey::generate();
    let host = start_host(share, &key).await;
    let ep = guest_endpoint(key).await;
    let (conn, info) = tokio::time::timeout(WAIT, guest::connect_once(&ep, &host.addr, None))
        .await
        .unwrap()
        .unwrap();
    // The hello reply carries only a short summary.
    assert_eq!(
        info.mods,
        Some(ModsOffer {
            files: 3,
            total_size: (big.len() + small.len()) as u64
        })
    );

    let manifest = tokio::time::timeout(WAIT, mods::fetch_manifest(&conn))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(manifest, host.share.manifest());
    assert_eq!(manifest.optional, ["c".repeat(64)]);

    for (entry, expected) in manifest.mods.iter().zip([&big[..], &small[..], b""]) {
        let part = guest_dir.join(format!("{}.part", entry.file_name));
        let mut steps = Vec::new();
        tokio::time::timeout(
            WAIT,
            mods::fetch_file(&conn, entry, &part, |done| steps.push(done)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            std::fs::read(&part).unwrap(),
            expected,
            "{}",
            entry.file_name
        );
        // Download progress only grows and reaches exactly the file size.
        assert!(steps.windows(2).all(|pair| pair[0] < pair[1]), "{steps:?}");
        assert_eq!(steps.last().copied().unwrap_or(0), entry.size);
        if entry.size > 200_000 {
            assert!(steps.len() > 3, "a big file goes in chunks: {steps:?}");
        }
    }

    // The host sees what it served and how much. The guest created the empty file itself, without a request.
    let sent = host.wait_sent(2).await;
    assert_eq!(
        sent,
        [
            ("big-pack.jar".to_string(), big.len() as u64),
            ("Маленький мод [1.20.1].jar".to_string(), small.len() as u64),
        ]
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn interrupted_download_resumes_and_corruption_is_caught() {
    let dir = temp_dir("resume");
    let data = content(300_000, 3);
    let share = ModShare::new();
    let shared = host_mod(&dir, "pack.jar", &data);
    let entry = shared.entry.clone();
    share.set(vec![shared], Vec::new()).unwrap();

    let key = SecretKey::generate();
    let host = start_host(share, &key).await;
    let ep = guest_endpoint(key).await;
    let (conn, _) = guest::connect_once(&ep, &host.addr, None).await.unwrap();

    // The previous transfer dropped at 100,000 bytes: only the rest is downloaded.
    let part = dir.join("pack.jar.part");
    std::fs::write(&part, &data[..100_000]).unwrap();
    let mut first_step = None;
    mods::fetch_file(&conn, &entry, &part, |done| {
        first_step.get_or_insert(done);
    })
    .await
    .unwrap();
    assert_eq!(std::fs::read(&part).unwrap(), data);
    assert_eq!(
        first_step,
        Some(100_000),
        "already downloaded bytes count in the progress"
    );
    assert_eq!(host.wait_sent(1).await, [("pack.jar".to_string(), 200_000)]);

    // The file is already fully downloaded: the host is not needed.
    mods::fetch_file(&conn, &entry, &part, |_| {})
        .await
        .unwrap();
    assert_eq!(host.sent().len(), 1);

    // The start of the file is corrupted: the whole-file hash does not match, so the file is deleted.
    let mut spoiled = data[..100_000].to_vec();
    spoiled[5] ^= 0xff;
    std::fs::write(&part, &spoiled).unwrap();
    let err = mods::fetch_file(&conn, &entry, &part, |_| {})
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("хэш не совпал"), "{err:#}");
    assert!(!part.exists(), "a corrupted file does not remain");

    // A retry from scratch succeeds.
    mods::fetch_file(&conn, &entry, &part, |_| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(&part).unwrap(), data);

    // Junk longer than the mod itself also starts from scratch.
    std::fs::write(&part, content(400_000, 9)).unwrap();
    mods::fetch_file(&conn, &entry, &part, |_| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(&part).unwrap(), data);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn host_gives_only_shared_files_and_only_unchanged() {
    let dir = temp_dir("refuse");
    let data = content(50_000, 4);
    let share = ModShare::new();
    let shared = host_mod(&dir, "pack.jar", &data);
    let entry = shared.entry.clone();
    share.set(vec![shared], Vec::new()).unwrap();
    // The file is right there, but not included in the share.
    let private = host_mod(&dir, "private.jar", b"not shared").entry;

    let key = SecretKey::generate();
    let host = start_host(share.clone(), &key).await;
    let ep = guest_endpoint(key).await;
    let (conn, _) = guest::connect_once(&ep, &host.addr, None).await.unwrap();
    let part = dir.join("download.part");

    let err = mods::fetch_file(&conn, &private, &part, |_| {})
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("нет в раздаче"), "{err:#}");
    assert!(!part.exists());

    // The guest never requests a name leading out of the mods folder.
    let mut traversal = entry.clone();
    traversal.file_name = "../evil.jar".into();
    assert!(
        mods::fetch_file(&conn, &traversal, &part, |_| {})
            .await
            .is_err()
    );
    assert!(!part.exists());

    // The file on the host's disk was swapped (same size): the guest catches it by hash.
    let mut swapped = data.clone();
    swapped[0] ^= 1;
    std::fs::write(dir.join("pack.jar"), &swapped).unwrap();
    let err = mods::fetch_file(&conn, &entry, &part, |_| {})
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("хэш не совпал"), "{err:#}");
    assert!(!part.exists());

    // The size changed: the host refuses itself, and the answer has no paths of its PC.
    std::fs::write(dir.join("pack.jar"), b"shorter").unwrap();
    let err = format!(
        "{:#}",
        mods::fetch_file(&conn, &entry, &part, |_| {})
            .await
            .unwrap_err()
    );
    assert!(err.contains("изменился на ПК хоста"), "{err}");
    assert!(!err.contains(&dir.display().to_string()), "{err}");

    // The share was turned off: the list is empty, files are not served, a new guest sees "no mods".
    share.clear();
    assert!(mods::fetch_manifest(&conn).await.unwrap().mods.is_empty());
    assert!(
        mods::fetch_file(&conn, &entry, &part, |_| {})
            .await
            .is_err()
    );
    let (_conn, info) = guest::connect_once(&ep, &host.addr, None).await.unwrap();
    assert_eq!(info.mods, Some(ModsOffer::default()));
    std::fs::remove_dir_all(&dir).unwrap();
}
