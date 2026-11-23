//! Room styling in the game, from start to end: a guest joins the world (real `Handshake`
//! and `Login Start` packets through the tunnel) → the host learns their in-game nickname → the datapack in
//! the host's world gets the header, tags, colors and highlight → the room is closed and the datapack
//! cleans up after itself. The host and guests are on one PC (`127.0.0.1`), the world is a temporary folder.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use n0_future::boxed::BoxFuture;
use phase_core::{
    EventBus, GamePort, HostOptions, HostVoice, JoinOptions, LookCommand, LookMode, LookSettings,
    NetOptions, Phase, PlayerStyle, Session, SessionStatus, WorldSource, look,
};
use phase_minecraft::lan::AnnounceMode;
use phase_tunnel::{EndpointId, EndpointTicket, access::Approver, endpoint::NetworkMode, identity};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const WAIT: Duration = Duration::from_secs(30);

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "phase-look-it-{name}-{}-{}",
        std::process::id(),
        phase_tunnel::metrics::unix_now()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A world on disk "opened by the game": the `session.lock` lock is held while the file is alive.
struct OpenWorld {
    dir: PathBuf,
    _lock: fs::File,
}

impl OpenWorld {
    fn new(root: &Path) -> Self {
        let dir = root.join("saves").join("Test World");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("level.dat"), b"nbt").unwrap();
        Self::open(dir)
    }

    /// The game opened the same world again.
    fn open(dir: PathBuf) -> Self {
        fs::write(dir.join("session.lock"), "\u{2603}").unwrap();
        let lock = fs::OpenOptions::new()
            .write(true)
            .open(dir.join("session.lock"))
            .unwrap();
        lock.try_lock().unwrap();
        Self { dir, _lock: lock }
    }

    fn function(&self, name: &str) -> String {
        let path = self
            .dir
            .join("datapacks")
            .join("phase")
            .join("data")
            .join("phase")
            .join("function")
            .join(format!("{name}.mcfunction"));
        fs::read_to_string(path).unwrap_or_default()
    }

    /// Waits until the needed thing appears (or disappears) in the datapack function.
    async fn wait_function(&self, name: &str, what: &str, check: impl Fn(&str) -> bool) -> String {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let text = self.function(name);
            if check(&text) {
                return text;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{name}.mcfunction has no \"{what}\":\n{text}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// The "game": accepts connections and silently reads, like a server during login.
async fn fake_game() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while matches!(tcp.read(&mut buf).await, Ok(len) if len > 0) {}
            });
        }
    });
    port
}

fn varint(mut value: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn packet(body: &[u8]) -> Vec<u8> {
    [varint(body.len() as u32), body.to_vec()].concat()
}

/// The start of a 1.21.1 game login: `Handshake` (next state is login) and `Login Start`.
fn login_bytes(name: &str) -> Vec<u8> {
    let address = b"127.0.0.1";
    let handshake = [
        varint(0),
        varint(767),
        varint(address.len() as u32),
        address.to_vec(),
        25565u16.to_be_bytes().to_vec(),
        varint(2),
    ]
    .concat();
    let login = [
        varint(0),
        varint(name.len() as u32),
        name.as_bytes().to_vec(),
        vec![7u8; 16],
    ]
    .concat();
    [packet(&handshake), packet(&login)].concat()
}

/// The guest's game joins the world under nickname `name`. The connection is held while the socket is alive.
async fn enter_world(guest: &Session, name: &str) -> TcpStream {
    let addr = guest.status().guest.unwrap().local_addr;
    let mut game = TcpStream::connect(addr).await.unwrap();
    game.write_all(&login_bytes(name)).await.unwrap();
    game
}

#[derive(Debug)]
struct AllowAll;

impl Approver for AllowAll {
    fn approve(&self, _id: EndpointId, _nickname: Option<String>) -> BoxFuture<bool> {
        Box::pin(async { true })
    }
}

fn loopback() -> NetOptions {
    NetOptions {
        mode: NetworkMode::LoopbackOnly,
        ..NetOptions::default()
    }
}

fn host_options(dir: &Path, world: &OpenWorld, port: u16) -> HostOptions {
    let mut opts = HostOptions::new("host");
    opts.profile_dir = Some(dir.join("host"));
    opts.nickname = Some("HostNick".into());
    opts.game_port = Some(GamePort::Fixed(port));
    opts.voice = HostVoice::Off;
    opts.approver = Some(Arc::new(AllowAll));
    opts.net = loopback();
    opts.status_every = Duration::from_millis(300);
    opts.world = WorldSource::Dir(world.dir.clone());
    opts
}

fn join_options(dir: &Path, name: &str, ticket: &str) -> JoinOptions {
    let ticket: EndpointTicket = ticket.parse().unwrap();
    let mut opts = JoinOptions::new(name, ticket);
    opts.profile_dir = Some(dir.join(name));
    opts.nickname = Some(name.into());
    opts.announce = AnnounceMode::Off;
    opts.voice = false;
    opts.net = loopback();
    opts.status_every = Duration::from_millis(300);
    opts
}

async fn wait_status(session: &Session, what: impl Fn(&SessionStatus) -> bool) -> SessionStatus {
    let mut rx = session.watch_status();
    let status = tokio::time::timeout(WAIT, rx.wait_for(|s| what(s) || s.phase.is_finished()))
        .await
        .expect("the session did not reach the needed state")
        .unwrap()
        .clone();
    assert!(what(&status), "the session finished earlier: {status:?}");
    status
}

async fn running(session: &Session) -> SessionStatus {
    wait_status(session, |s| s.phase == Phase::Running).await
}

fn device_of(profile_dir: &Path) -> EndpointId {
    fs::create_dir_all(profile_dir).unwrap();
    identity::load_or_create_secret_key(profile_dir)
        .unwrap()
        .public()
}

#[tokio::test]
async fn room_shows_welcome_bar_host_tag_and_guests_toggle_their_own_glow() {
    let dir = temp_dir("room");
    let world = OpenWorld::new(&dir);

    let host = Session::host(
        host_options(&dir, &world, fake_game().await),
        EventBus::new(),
    );
    let ready = running(&host).await.host.unwrap();
    let code = look::room_code(&device_of(&dir.join("host")).to_string());
    assert_eq!(ready.look.code, code);
    assert_eq!(ready.look.title, "PHASE");

    // The room is open: the world has a header with the room code and the host tag, no guests yet.
    // There is no mod in the world: eight seats, as the game itself allows.
    let apply = world
        .wait_function("apply", "header", |text| text.contains("bossbar add"))
        .await;
    assert!(apply.contains(&format!(" • {code} • ")), "{apply}");
    assert!(apply.contains("bossbar set phase:room max 8\n"));
    assert!(
        apply.contains("schedule function phase:hide 200t\n"),
        "header for 10 seconds"
    );
    assert!(apply.contains("prefix {\"text\":\"[HOST] \",\"color\":\"#38BDF8\"}"));
    assert!(apply.contains("execute as @a[name=HostNick,team=] run team join ph.a00 @s"));
    // The datapack appeared in an already open world: the game will load it on the next open.
    let status = wait_status(&host, |s| {
        s.host.as_ref().is_some_and(|h| h.look.world.is_some())
    })
    .await;
    let look_status = status.host.unwrap().look;
    assert_eq!(look_status.mode, LookMode::WaitingReload);
    assert_eq!(look_status.world.as_deref(), Some("Test World"));
    assert!(look_status.players[0].host && look_status.players[0].styled);

    // Guests: in the Phase window they are "steve_pc" and "alex_pc", they join the game as Steve_01 and Alex.
    let steve = Session::join(
        join_options(&dir, "steve_pc", &ready.ticket),
        EventBus::new(),
    );
    running(&steve).await;
    let _steve_game = enter_world(&steve, "Steve_01").await;
    let alex = Session::join(
        join_options(&dir, "alex_pc", &ready.ticket),
        EventBus::new(),
    );
    running(&alex).await;
    let _alex_game = enter_world(&alex, "Alex").await;

    let status = wait_status(&host, |s| {
        s.host.as_ref().is_some_and(|h| {
            h.look.players.len() == 3 && h.look.players.iter().all(|p| p.game_name.is_some())
        })
    })
    .await;
    let players = status.host.unwrap().look.players;
    let alex_row = players
        .iter()
        .find(|p| p.nickname.as_deref() == Some("alex_pc"))
        .unwrap();
    assert_eq!(
        alex_row.game_name.as_deref(),
        Some("Alex"),
        "nickname from the game, not from Phase"
    );
    // A guest without a style from the host is not styled.
    assert_eq!(
        (alex_row.label.as_deref(), alex_row.color.as_deref()),
        (None, None)
    );
    let steve_id = players
        .iter()
        .find(|p| p.game_name.as_deref() == Some("Steve_01"))
        .unwrap()
        .device_id
        .clone();

    // The host styles the guest: free, Phase Pro is not needed.
    assert!(host.look_command(LookCommand::Style {
        device_id: steve_id,
        style: PlayerStyle {
            color: Some("coral_flame".into()),
            prefix: Some("VIP".into()),
            glow: false,
        },
    }));
    let apply = world
        .wait_function("apply", "Steve_01", |text| text.contains("name=Steve_01,"))
        .await;
    assert!(apply.contains("{\"text\":\"[VIP] \""), "{apply}");
    assert!(!apply.contains("name=Alex,"), "Alex has no styling");

    // The guest turns the highlight on for themselves: the host allows this by default.
    assert!(alex.set_own_glow(true));
    let glow = world
        .wait_function("glow", "Alex highlight", |text| {
            text.contains("effect give @a[name=Alex]")
        })
        .await;
    assert!(glow.contains("effect give @a[name=Alex] minecraft:glowing 999999 0 true\n"));
    assert!(!glow.contains("effect give @a[name=Steve_01]"));
    let seen = wait_status(&alex, |s| {
        s.guest
            .as_ref()
            .and_then(|g| g.look.as_ref())
            .is_some_and(|l| l.glow)
    })
    .await
    .guest
    .unwrap()
    .look
    .unwrap();
    assert!(seen.enabled && seen.can_glow);
    assert_eq!(seen.game_name.as_deref(), Some("Alex"));
    assert_eq!((seen.color, seen.prefix), (None, None));

    // The host forbade guests the highlight and removed the styles: the highlight goes out, and the guest
    // can no longer toggle it.
    assert!(host.look_command(LookCommand::Settings(LookSettings {
        guest_glow: false,
        ..LookSettings::default()
    })));
    world
        .wait_function("glow", "highlight off", |text| {
            !text.contains("effect give")
        })
        .await;
    assert!(alex.set_own_glow(true));
    let denied = wait_status(&alex, |s| {
        s.guest
            .as_ref()
            .and_then(|g| g.look.as_ref())
            .is_some_and(|l| !l.can_glow)
    })
    .await
    .guest
    .unwrap()
    .look
    .unwrap();
    assert!(!denied.glow);

    // Guests joining and leaving do not call `reload` (a lag spike on modded builds): `changed`
    // only applies the styling, and `reload` stayed in the manual `/trigger phase.sync`.
    let changed = world.function("changed");
    assert!(changed.ends_with("function phase:apply\n"), "{changed}");
    assert!(!changed.contains("reload") && !changed.contains("schedule"));
    assert!(world.function("sync").is_empty(), "no deferred sync");
    assert!(world.function("manual").ends_with("reload\n"));

    // The room is closed: the datapack cleans up after itself and goes to sleep.
    alex.shutdown().await.unwrap();
    steve.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
    let apply = world.function("apply");
    assert!(apply.contains("bossbar remove phase:room\n"), "{apply}");
    assert!(apply.contains("team remove ph.a00\n"));
    assert!(!apply.contains("HostNick") && !apply.contains("bossbar add"));
    assert!(!world.function("glow").contains("effect"));
    let room_json = fs::read_to_string(world.dir.join("phase").join("room.json")).unwrap();
    assert!(room_json.contains("\"active\": false"));
    // The host's settings are saved in their profile.
    assert!(!look::load_settings(&dir.join("host")).guest_glow);
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn reopening_the_same_world_keeps_the_room_look() {
    // The host left to the menu and at once opened the world again: the game reads the datapack at the moment of opening,
    // so while the room is open the world must hold the working version, not a cleanup.
    let dir = temp_dir("reopen");
    let world = OpenWorld::new(&dir);
    let world_dir = world.dir.clone();
    let host = Session::host(
        host_options(&dir, &world, fake_game().await),
        EventBus::new(),
    );
    running(&host).await;
    world
        .wait_function("apply", "header", |text| text.contains("bossbar add"))
        .await;
    wait_status(&host, |s| {
        s.host
            .as_ref()
            .is_some_and(|h| h.look.mode == LookMode::WaitingReload)
    })
    .await;

    drop(world);
    wait_status(&host, |s| {
        s.host
            .as_ref()
            .is_some_and(|h| h.look.mode == LookMode::NoWorld)
    })
    .await;
    let world = OpenWorld::open(world_dir);
    let apply = world.function("apply");
    assert!(
        apply.contains("bossbar add"),
        "the world was closed, styling removed:\n{apply}"
    );
    assert!(apply.contains("scoreboard players enable @a[name=HostNick] phase.sync"));
    // The files were in the world before opening: the game loaded them.
    wait_status(&host, |s| {
        s.host
            .as_ref()
            .is_some_and(|h| h.look.mode == LookMode::Active)
    })
    .await;

    // The room is closed: cleanup, as before.
    host.shutdown().await.unwrap();
    assert!(
        world
            .function("apply")
            .contains("bossbar remove phase:room\n")
    );
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn companion_lifts_the_room_to_the_host_setting() {
    let dir = temp_dir("companion");
    let world = OpenWorld::new(&dir);
    // Phase Companion checked in the world after it was opened.
    tokio::time::sleep(Duration::from_millis(50)).await;
    fs::create_dir_all(world.dir.join("phase")).unwrap();
    fs::write(world.dir.join("phase").join("companion.ok"), "ok").unwrap();
    let room_json = world.dir.join("phase").join("room.json");
    let max_in_room_json = |wanted: u32| {
        fs::read_to_string(&room_json)
            .is_ok_and(|text| text.contains(&format!("\"max_players\": {wanted},")))
    };

    let host = Session::host(
        host_options(&dir, &world, fake_game().await),
        EventBus::new(),
    );
    let ready = running(&host).await.host.unwrap();
    assert_eq!(
        ready.room.max_players, 8,
        "until the mod is found: as in the game"
    );

    // The mod is found: 20 seats by default, the mod gets the same in `room.json`.
    let status = wait_status(&host, |s| {
        s.host
            .as_ref()
            .is_some_and(|h| h.room.max_players == 20 && h.look.mode == LookMode::Companion)
    })
    .await;
    let settings = status.host.unwrap().look.settings;
    for _ in 0..100 {
        if max_in_room_json(20) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(max_in_room_json(20), "{:?}", fs::read_to_string(&room_json));

    // The host raised it to 50: neither Phase nor the mod allows more.
    assert!(host.look_command(LookCommand::Settings(LookSettings {
        slots: 50,
        ..settings
    })));
    wait_status(&host, |s| {
        s.host.as_ref().is_some_and(|h| h.room.max_players == 50)
    })
    .await;
    for _ in 0..100 {
        if max_in_room_json(50) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(max_in_room_json(50), "{:?}", fs::read_to_string(&room_json));

    host.shutdown().await.unwrap();
    let _ = fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn host_paints_names_sets_prefixes_title_and_glow_on_the_fly() {
    let dir = temp_dir("styles");
    let world = OpenWorld::new(&dir);
    let host_dir = dir.join("host");
    // Settings from the previous room: a custom header title.
    look::save_settings(
        &host_dir,
        &LookSettings {
            title: Some("Vanilla Crew".into()),
            ..LookSettings::default()
        },
    )
    .unwrap();

    let host = Session::host(
        host_options(&dir, &world, fake_game().await),
        EventBus::new(),
    );
    let ready = running(&host).await.host.unwrap();
    assert_eq!(ready.look.title, "Vanilla Crew");
    let host_id = ready.look.players[0].device_id.clone();

    let guest = Session::join(join_options(&dir, "friend", &ready.ticket), EventBus::new());
    running(&guest).await;
    let _game = enter_world(&guest, "Friend_7").await;
    let status = wait_status(&host, |s| {
        s.host.as_ref().is_some_and(|h| {
            h.look
                .players
                .iter()
                .any(|p| p.game_name.as_deref() == Some("Friend_7"))
        })
    })
    .await;
    let friend_id = status
        .host
        .unwrap()
        .look
        .players
        .into_iter()
        .find(|p| !p.host)
        .unwrap()
        .device_id;

    // A custom header title and the [HOST] tag are available to any host, no Phase Pro.
    let apply = world
        .wait_function("apply", "custom title", |text| {
            text.contains("Vanilla Crew")
        })
        .await;
    assert!(apply.contains("bossbar set phase:room max 8\n"), "{apply}");
    assert!(
        !apply.contains("team modify ph.a00 color"),
        "the host has not chosen their own color yet"
    );
    assert!(apply.contains("prefix {\"text\":\"[HOST] \",\"color\":\"#38BDF8\"}"));
    assert!(!apply.contains("Friend_7"), "the guest has no styling yet");

    // The host styles a friend, gives a prefix and highlight: the datapack changes on the fly.
    assert!(host.look_command(LookCommand::Style {
        device_id: friend_id.clone(),
        style: PlayerStyle {
            color: Some("emerald_mint".into()),
            prefix: Some("Билдер".into()),
            glow: true,
        },
    }));
    // And themselves too.
    assert!(host.look_command(LookCommand::Style {
        device_id: host_id,
        style: PlayerStyle {
            color: Some("crimson_pulse".into()),
            prefix: Some("KING".into()),
            glow: false,
        },
    }));
    let apply = world
        .wait_function("apply", "styles", |text| {
            text.contains("[Билдер] ") && text.contains("[KING] ")
        })
        .await;
    assert!(
        apply.contains("team modify ph.a00 color dark_red\n"),
        "{apply}"
    );
    assert!(
        apply.contains("team modify ph.a00 prefix {\"text\":\"[KING] \",\"color\":\"#DC2626\"}\n")
    );
    assert!(apply.contains("team modify ph.c01 color green\n"));
    assert!(
        apply
            .contains("team modify ph.c01 prefix {\"text\":\"[Билдер] \",\"color\":\"#34D399\"}\n")
    );
    assert!(apply.contains("execute as @a[name=Friend_7,team=] run team join ph.c01 @s\n"));
    world
        .wait_function("glow", "friend highlight", |text| {
            text.contains("name=Friend_7]")
        })
        .await;

    // The friend sees their own style in their window and turns the highlight off for themselves.
    let seen = wait_status(&guest, |s| {
        s.guest
            .as_ref()
            .and_then(|g| g.look.as_ref())
            .is_some_and(|l| l.glow)
    })
    .await
    .guest
    .unwrap()
    .look
    .unwrap();
    assert!(
        seen.can_glow,
        "the host allows guests the highlight by default"
    );
    assert_eq!(seen.color.as_deref(), Some("emerald_mint"));
    assert_eq!(seen.prefix.as_deref(), Some("Билдер"));
    assert!(guest.set_own_glow(false));
    world
        .wait_function(
            "glow",
            "подсветка выключена гостем",
            |text| !text.contains("effect give"),
        )
        .await;
    // The host sees this in their panel; the friend's color and prefix stayed.
    let status = wait_status(&host, |s| {
        s.host
            .as_ref()
            .is_some_and(|h| h.look.players.iter().any(|p| !p.host && !p.glow))
    })
    .await;
    let friend = status
        .host
        .unwrap()
        .look
        .players
        .into_iter()
        .find(|p| !p.host)
        .unwrap();
    assert_eq!(friend.label.as_deref(), Some("Билдер"));
    assert_eq!(friend.color.as_deref(), Some("emerald_mint"));

    // The host turned styling off entirely: the world has a cleanup, the room keeps working.
    let mut settings = look::load_settings(&host_dir);
    assert_eq!(
        settings.styles[&friend_id].prefix.as_deref(),
        Some("Билдер")
    );
    settings.enabled = false;
    assert!(host.look_command(LookCommand::Settings(settings)));
    let apply = world
        .wait_function("apply", "cleanup", |text| text.contains("bossbar remove"))
        .await;
    assert!(apply.contains("team remove ph.c01\n") && !apply.contains("Friend_7"));
    let status = wait_status(&host, |s| {
        s.host
            .as_ref()
            .is_some_and(|h| h.look.mode == LookMode::Off)
    })
    .await;
    assert_eq!(status.host.unwrap().guests.len(), 1);

    guest.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
    let _ = fs::remove_dir_all(&dir);
}

/// All files of the folder (path, contents): a snapshot of "how it lay in the world at the moment of the crash".
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    out
}

#[tokio::test]
async fn leftovers_after_a_crash_are_cleaned_at_the_next_start() {
    let dir = temp_dir("crash");
    let world = OpenWorld::new(&dir);
    let host_dir = dir.join("host");
    let pack = world.dir.join("datapacks").join("phase");

    // A normal session: an active datapack in the world, the world is recorded in the profile list.
    let host = Session::host(
        host_options(&dir, &world, fake_game().await),
        EventBus::new(),
    );
    running(&host).await;
    world
        .wait_function("apply", "header", |text| text.contains("bossbar add"))
        .await;
    // The state file is written after the functions: we wait for it so the snapshot is whole.
    for _ in 0..100 {
        if fs::read_to_string(pack.join("phase-state.json"))
            .is_ok_and(|text| text.contains("\"active\": true"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut residue = snapshot(&pack);
    residue.extend(snapshot(&world.dir.join("phase")));
    residue.extend(snapshot(&host_dir).into_iter().filter(|(path, _)| {
        path.file_name()
            .is_some_and(|name| name == look::WORLDS_FILE)
    }));
    assert!(
        residue
            .iter()
            .any(|(path, _)| path.ends_with(look::WORLDS_FILE)),
        "the world is not recorded in the profile list"
    );
    host.shutdown().await.unwrap();

    // A crash: Phase did not clean up after itself; we restore the files as they were at the crash and
    // add `tick` files of the old version.
    for (path, bytes) in &residue {
        fs::write(path, bytes).unwrap();
    }
    for stale in ["tag", "function"] {
        let path = match stale {
            "tag" => pack.join("data/minecraft/tags/function/tick.json"),
            _ => pack.join("data/phase/function/tick.mcfunction"),
        };
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "stale").unwrap();
    }
    let crashed = world.function("apply");
    assert!(crashed.contains("bossbar add") && !crashed.contains("bossbar remove"));
    assert!(
        fs::read_to_string(pack.join("phase-state.json"))
            .unwrap()
            .contains("\"active\": true")
    );

    // The next Phase start (the world is not written to now): leftovers are removed.
    let mut opts = host_options(&dir, &world, fake_game().await);
    opts.world = WorldSource::Off;
    let host = Session::host(opts, EventBus::new());
    running(&host).await;
    let apply = world
        .wait_function("apply", "cleanup", |text| text.contains("bossbar remove"))
        .await;
    assert!(apply.contains("team remove ph.a00\n"), "{apply}");
    assert!(apply.contains("effect clear @a minecraft:glowing\n"));
    assert!(!apply.contains("bossbar add") && !apply.contains("HostNick"));
    // `glow` is written right after `apply`, so it must be awaited too.
    world
        .wait_function("glow", "dormant glow", |text| {
            !text.is_empty() && !text.contains("effect")
        })
        .await;
    // State and deletion of old files come after the functions: we wait for the end of the write.
    for _ in 0..100 {
        if fs::read_to_string(pack.join("phase-state.json"))
            .is_ok_and(|text| text.contains("\"active\": false"))
            && !host_dir.join(look::WORLDS_FILE).exists()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        fs::read_to_string(pack.join("phase-state.json"))
            .unwrap()
            .contains("\"active\": false")
    );
    assert!(!pack.join("data/minecraft/tags/function/tick.json").exists());
    assert!(!pack.join("data/phase/function/tick.mcfunction").exists());
    // The dormant skeleton is in place, the world list is empty, the room for the mod is closed.
    assert!(
        world
            .function("load")
            .contains("schedule function phase:watch")
    );
    assert!(!host_dir.join(look::WORLDS_FILE).exists());
    let room_json = fs::read_to_string(world.dir.join("phase").join("room.json")).unwrap();
    assert!(room_json.contains("\"active\": false"));

    host.shutdown().await.unwrap();
    let _ = fs::remove_dir_all(&dir);
}
