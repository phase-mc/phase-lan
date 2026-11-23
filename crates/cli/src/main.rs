//! `phase-cli`: the console version of Phase.
//!
//! Commands:
//! - `id`: show this device's ID;
//! - `nick`: show the profile's game nickname (the global `--nick` option sets it);
//! - `host`: host, share your world/server with friends (prints a ticket);
//! - `join`: guest, connect by ticket and get a local address for Minecraft;
//! - `echo`: channel check without Minecraft, a ping through the tunnel.
//!
//! The `host` and `join` flows live in `phase-core` (the app window uses them too).
//! Only the console "interface" is here: command-line options, printing state
//! and events, the "let in?" question from the keyboard.

use std::{
    io::IsTerminal,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use n0_future::boxed::BoxFuture;
use phase_core::{
    EventBus, GamePort, GuestMods, GuestReady, HostMods, HostOptions, HostReady, HostVoice,
    JoinOptions, Level, ModCommand, ModSyncState, ModrinthSource, ModsFolder, NetOptions, Phase,
    RelayInfo, Session, SessionEvent, SessionStatus, VoiceMode, reports, with_relay_hint,
};
use phase_lang::tr;
use phase_minecraft::lan::AnnounceMode;
use phase_tunnel::{
    EndpointId, EndpointTicket, RelayUrl,
    access::Approver,
    echo,
    endpoint::{self, EndpointOptions, NetworkMode},
    guest, identity,
    metrics::{self, Telemetry},
    wire,
};
use tokio::sync::broadcast::{self, error::RecvError};
use tracing::{info, warn};

/// Prints a line in the system language: `say!("по-русски", "in English")`.
macro_rules! say {
    ($ru:expr, $en:expr) => {
        println!("{}", tr!($ru, $en))
    };
}

#[derive(Parser)]
#[command(
    name = "phase-cli",
    version,
    about = "Phase: play Minecraft Java Edition over the internet as if on a LAN (prototype)"
)]
struct Cli {
    /// Profile: a separate device key and allow list.
    /// Needed to run a host and a guest on one PC (for example --profile guest).
    #[arg(long, global = true, default_value = phase_core::DEFAULT_PROFILE)]
    profile: String,

    /// Game nickname: how friends see you (in the "let in?" question and the guest list).
    /// Saved in the profile. Without the option, the profile's nickname; if that is not set, the program
    /// takes the nickname from Minecraft on this PC.
    #[arg(long, global = true, value_name = "NICK", value_parser = parse_nick)]
    nick: Option<String>,

    /// How often to print the ping and path type, in seconds.
    #[arg(long, global = true, default_value_t = 5)]
    status_secs: u64,

    /// Verbose log (for debugging).
    #[arg(long, short, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show this device's ID (the host can add it to the allowed list).
    Id,
    /// Show the profile's game nickname (set it: --nick <NICK>).
    Nick,
    /// Host: open your world or server to friends.
    Host(HostArgs),
    /// Guest: connect to a host by ticket.
    Join(JoinArgs),
    /// Channel check without Minecraft: measure the ping through the tunnel.
    Echo(EchoArgs),
}

#[derive(Args)]
struct NetArgs {
    /// Relay only, no direct connection (test T4).
    #[arg(long, conflicts_with = "loopback_only")]
    relay_only: bool,
    /// 127.0.0.1 only, without internet: a check on one PC.
    #[arg(long)]
    loopback_only: bool,
    /// Your own relay server (the phase-relay program) instead of the free n0 relays,
    /// for example https://relay.example.com. Can be given several times. The guest does not
    /// need it: it takes the host's relay from the ticket.
    #[arg(long = "relay", value_name = "URL", value_parser = parse_relay, conflicts_with = "loopback_only")]
    relay: Vec<RelayUrl>,
    /// Password of your own relay, if the relay requires one (both host and guest need it).
    /// Can be set with the PHASE_RELAY_TOKEN environment variable.
    #[arg(long, value_name = "PASSWORD", conflicts_with = "loopback_only")]
    relay_token: Option<String>,
    /// Relay certificate file (PEM), if the relay has a self-signed certificate
    /// (relay-cert.pem from the relay owner).
    #[arg(long, value_name = "FILE", conflicts_with = "loopback_only")]
    relay_ca: Option<PathBuf>,
}

fn parse_nick(text: &str) -> Result<String, String> {
    phase_core::profiles::validate_nickname(text).map_err(|err| format!("{err:#}"))
}

fn parse_relay(text: &str) -> Result<RelayUrl, String> {
    endpoint::parse_relay_url(text).map_err(|err| format!("{err:#}"))
}

impl NetArgs {
    fn options(&self) -> NetOptions {
        NetOptions {
            mode: if self.loopback_only {
                NetworkMode::LoopbackOnly
            } else if self.relay_only {
                NetworkMode::RelayOnly
            } else {
                NetworkMode::Normal
            },
            relays: self.relay.clone(),
            relay_token: self.relay_token.clone(),
            relay_ca: self.relay_ca.clone(),
        }
    }
}

#[derive(Args)]
struct HostArgs {
    /// Port of the world or server on this PC (the game prints it after "Open to LAN")
    /// or `auto` to find an open LAN world automatically.
    /// Without this option only the channel check (echo) works.
    #[arg(long, short)]
    port: Option<GamePort>,
    /// World name for friends (defaults to the name of the found world).
    #[arg(long)]
    motd: Option<String>,
    /// UDP ports of voice mods, comma-separated.
    /// By default the program finds them itself (the game log and the game process ports)
    /// and adds fallbacks: the game port and 24454.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "PORTS",
        conflicts_with = "no_udp"
    )]
    udp: Vec<u16>,
    /// Do not forward voice (UDP).
    #[arg(long)]
    no_udp: bool,
    /// Game or server folder (the one containing the logs folder), used to find voice ports
    /// in the log. Needed only if the launcher keeps the game in an unusual place.
    /// Can be given several times.
    #[arg(long, value_name = "DIR")]
    game_dir: Vec<PathBuf>,
    /// Allow a device in advance (its ID from the `id` command). Can be given several times.
    #[arg(long = "allow", value_name = "ID")]
    allow: Vec<EndpointId>,
    /// Do not ask about new devices, let in only allowed ones.
    #[arg(long)]
    no_prompt: bool,
    /// Share mods from your mods folder with guests: a guest compares its folder and downloads
    /// the missing .jar files (from Modrinth first, otherwise directly from this PC).
    #[arg(long)]
    share_mods: bool,
    /// Mods folder to share. Defaults to the running game's folder, or if the game is not
    /// running, %APPDATA%\.minecraft\mods.
    #[arg(long, value_name = "DIR", requires = "share_mods")]
    mods_dir: Option<PathBuf>,
    /// Do not share this file (a purely client-side mod: shaders, replay recording).
    /// Can be given several times.
    #[arg(long = "no-share", value_name = "FILE.jar", requires = "share_mods")]
    no_share: Vec<String>,
    #[command(flatten)]
    net: NetArgs,
}

#[derive(Args)]
struct JoinArgs {
    /// Ticket from the host.
    ticket: EndpointTicket,
    /// Local port for Minecraft (defaults to any free one).
    #[arg(long)]
    local_port: Option<u16>,
    /// How to show the world in the "LAN" list: unicast (default),
    /// multicast, both or off.
    #[arg(long, default_value = "unicast")]
    announce: AnnounceMode,
    /// Do not forward voice (UDP).
    #[arg(long)]
    no_voice: bool,
    /// Sync mods with the host right after connecting: extra .jar files move to
    /// mods\.phase-backup, missing ones are downloaded. Without the option the program only
    /// reports how the mods differ.
    #[arg(long, conflicts_with = "restore_mods")]
    sync_mods: bool,
    /// Restore your own mods from mods\.phase-backup and remove the host's mods.
    #[arg(long)]
    restore_mods: bool,
    /// Your mods folder. Defaults to the running game's folder, or if the game is not
    /// running, %APPDATA%\.minecraft\mods.
    #[arg(long, value_name = "DIR")]
    mods_dir: Option<PathBuf>,
    /// Do not look for mods on Modrinth, download only from the host.
    #[arg(long)]
    no_modrinth: bool,
    #[command(flatten)]
    net: NetArgs,
}

/// Mod hash cache file: `%LOCALAPPDATA%\Phase\mod-hashes.json`.
fn mod_hash_cache() -> Option<PathBuf> {
    identity::data_dir()
        .ok()
        .map(|dir| dir.join("mod-hashes.json"))
}

#[derive(Args)]
struct EchoArgs {
    /// Ticket from the host.
    ticket: EndpointTicket,
    /// How many measurements to take.
    #[arg(long, default_value_t = 100)]
    count: u32,
    /// Interval between measurements, ms.
    #[arg(long, default_value_t = 100)]
    interval_ms: u64,
    #[command(flatten)]
    net: NetArgs,
}

/// Console version reports are saved to the current folder.
fn reports_dir() -> PathBuf {
    PathBuf::new()
}

#[tokio::main]
async fn main() -> Result<()> {
    // Output language is the Windows language (`PHASE_LANG=ru|en` overrides). The clap help (`--help`)
    // stays English: its texts are doc comments, baked in at build time.
    phase_lang::init_from_system();
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let status_every = Duration::from_secs(cli.status_secs.max(1));
    if let Some(nick) = &cli.nick {
        // The nickname is saved in the profile: next time --nick can be omitted.
        phase_core::profiles::set_nickname(&cli.profile, nick)?;
    }

    match cli.command {
        Command::Id => {
            println!("{}", phase_core::profiles::device_id(&cli.profile)?);
            Ok(())
        }
        Command::Nick => {
            println!("{}", phase_core::profiles::nickname(&cli.profile)?);
            Ok(())
        }
        Command::Host(args) => run_host(cli.profile, args, status_every).await,
        Command::Join(args) => run_join(cli.profile, args, status_every).await,
        Command::Echo(args) => run_echo(&cli.profile, args, status_every).await,
    }
}

fn init_logging(verbose: bool) {
    use tracing_subscriber::EnvFilter;
    let default = if verbose {
        "phase_tunnel=debug,phase_core=debug,phase_cli=debug,iroh=info,warn"
    } else {
        "phase_tunnel=info,phase_core=info,phase_cli=info,warn"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::uptime())
        // No color codes: the old PowerShell window shows them as "←[2m" junk.
        .with_ansi(false)
        // The log goes to stderr: stdout keeps only the program's answers (for example, the ID from
        // the `id` command), so they can be copied and used in scripts.
        .with_writer(std::io::stderr)
        .init();
}

async fn run_host(profile: String, args: HostArgs, status_every: Duration) -> Result<()> {
    let approver: Option<Arc<dyn Approver>> = if args.no_prompt {
        None
    } else {
        if !std::io::stdin().is_terminal() {
            warn!(
                "{}",
                tr!(
                    "ввод с клавиатуры недоступен (программа запущена не в обычном окне консоли): \
                     на вопрос «пустить?» ответить не получится. Разрешайте устройства заранее: --allow <ID>",
                    "keyboard input is unavailable (the program is not running in a normal console window): \
                     the \"let them in?\" question cannot be answered. Allow devices in advance: --allow <ID>"
                )
            );
        }
        Some(Arc::new(ConsoleApprover::new()))
    };
    let mut opts = HostOptions::new(profile);
    opts.game_port = args.port;
    opts.motd = args.motd;
    opts.voice = if args.no_udp {
        HostVoice::Off
    } else if !args.udp.is_empty() {
        HostVoice::Manual(args.udp)
    } else {
        HostVoice::Auto {
            game_dirs: args.game_dir,
        }
    };
    opts.allow = args.allow;
    opts.approver = approver;
    opts.net = args.net.options();
    opts.status_every = status_every;
    opts.reports_dir = Some(reports_dir());
    opts.mods = HostMods {
        share: args.share_mods,
        folder: ModsFolder {
            chosen: args.mods_dir,
            ..ModsFolder::default()
        },
        excluded: args.no_share,
    };
    opts.mod_hash_cache = mod_hash_cache();

    let bus = EventBus::new();
    let events = bus.subscribe();
    run_session(Session::host(opts, bus), events, None).await
}

async fn run_join(profile: String, args: JoinArgs, status_every: Duration) -> Result<()> {
    let mut opts = JoinOptions::new(profile, args.ticket);
    opts.local_port = args.local_port;
    opts.announce = args.announce;
    opts.voice = !args.no_voice;
    opts.net = args.net.options();
    opts.status_every = status_every;
    opts.reports_dir = Some(reports_dir());
    opts.mods = GuestMods {
        folder: ModsFolder {
            chosen: args.mods_dir,
            ..ModsFolder::default()
        },
        auto: args.sync_mods,
        // In "this PC only" mode there is no internet by definition.
        modrinth: (!args.no_modrinth && !args.net.loopback_only).then(ModrinthSource::default),
    };
    opts.mod_hash_cache = mod_hash_cache();

    let bus = EventBus::new();
    let events = bus.subscribe();
    let first = args.restore_mods.then_some(ModCommand::Restore);
    run_session(Session::join(opts, bus), events, first).await
}

/// Console "interface" of a session: prints events and state while the session runs;
/// Ctrl+C stops it (connections are closed, the report is saved).
///
/// `on_ready` is the mod action the guest asks for right after connecting
/// (`--restore-mods`).
async fn run_session(
    mut session: Session,
    mut events: broadcast::Receiver<SessionEvent>,
    on_ready: Option<ModCommand>,
) -> Result<()> {
    let mut status = session.watch_status();
    let mut ready_printed = false;
    let mut mods_hint_printed = false;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let mut stopping = false;
    // The event bus closes when the session has ended (all senders dropped).
    // After that it must not be polled: `recv` returns "closed" right away, and the loop would spin
    // idle without reaching the state check, so the program would never exit.
    let mut events_open = true;
    loop {
        tokio::select! {
            // Events first: they are sent before the state changes, so they
            // are printed in the order they happened.
            biased;
            event = events.recv(), if events_open => match event {
                Ok(event) => print_event(&event),
                Err(RecvError::Lagged(n)) => warn!("пропущено событий: {n}"),
                Err(RecvError::Closed) => events_open = false,
            },
            changed = status.changed() => {
                if changed.is_err() {
                    break;
                }
                let current = status.borrow_and_update().clone();
                if !ready_printed && current.phase == Phase::Running {
                    ready_printed = true;
                    print_ready(&current);
                    if let Some(command) = on_ready {
                        session.mod_command(command);
                    }
                }
                // The console has no "Sync" button, so suggest the option.
                let mods = current.guest.as_ref().map(|guest| &guest.mods);
                if !mods_hint_printed
                    && on_ready.is_none()
                    && mods.is_some_and(|m| m.state == ModSyncState::NeedsSync && !m.auto)
                {
                    mods_hint_printed = true;
                    say!(
                        "Моды не менялись. Чтобы получить моды хоста, подключитесь с параметром \
                         --sync-mods: лишние моды переедут в mods\\.phase-backup (вернуть: \
                         --restore-mods).",
                        "Mods were not changed. To get the host's mods, join with --sync-mods: \
                         your extra mods will be moved to mods\\.phase-backup (to restore them: \
                         --restore-mods)."
                    );
                }
                if current.phase.is_finished() {
                    break;
                }
            },
            _ = &mut ctrl_c, if !stopping => {
                info!("остановка…");
                session.stop();
                stopping = true;
            },
        }
    }
    // Everything the session reported before ending (for example, "report saved").
    while let Ok(event) = events.try_recv() {
        print_event(&event);
    }
    session.wait().await
}

/// Prints an event. The core has already logged warnings (stderr), so they are not repeated;
/// network core events are in the log too.
fn print_event(event: &SessionEvent) {
    match event {
        SessionEvent::Notice {
            level: Level::Info,
            text,
        } => println!("{text}"),
        SessionEvent::Notice { .. } | SessionEvent::Tunnel(_) => {}
        SessionEvent::WorldSearch { timeout_s } => say!(
            format!("Ищу открытый LAN-мир на этом ПК (до {timeout_s} с)…"),
            format!("Looking for an open LAN World on this PC (up to {timeout_s} s)…")
        ),
        SessionEvent::WorldFound {
            name,
            port,
            summary,
        } => say!(
            format!("Найден мир «{name}» на порту {port} ({summary})."),
            format!("Found the world \"{name}\" on port {port} ({summary}).")
        ),
        SessionEvent::GameChecked {
            port,
            summary: Some(summary),
            ..
        } => say!(
            format!("На порту {port} отвечает сервер Minecraft: {summary}."),
            format!("A Minecraft server is responding on port {port}: {summary}.")
        ),
        SessionEvent::GameChecked { .. } => {}
        SessionEvent::WorldMoved {
            from,
            to,
            name,
            followed: true,
            ..
        } => say!(
            format!(
                "Мир «{name}» снова открыт — теперь на порту {to} (был {from}). Комната та же: \
                 друзья войдут по прежнему приглашению."
            ),
            format!(
                "The world \"{name}\" is open again, now on port {to} (was {from}). Same room: \
                 friends can join with the same invite."
            )
        ),
        SessionEvent::WorldMoved { from, to, name, .. } => say!(
            format!(
                "Мир «{name}» открыт на порту {to}, а комната ведёт на порт {from}, заданный \
                 вручную. Перезапустите хоста с --port {to} или --port auto."
            ),
            format!(
                "The world \"{name}\" is open on port {to}, but the room points to port {from}, \
                 which was set manually. Restart the host with --port {to} or --port auto."
            )
        ),
        // The console version asks itself (ConsoleApprover), no question queue is needed.
        SessionEvent::ApprovalRequested { .. } | SessionEvent::ApprovalClosed { .. } => {}
        SessionEvent::ReportSaved { path } => say!(
            format!("Отчёт сохранён: {path}"),
            format!("Report saved: {path}")
        ),
    }
}

fn print_ready(status: &SessionStatus) {
    if let Some(host) = &status.host {
        print_host_ready(host);
    }
    if let Some(guest) = &status.guest {
        print_guest_ready(guest);
    }
    if let Some(nickname) = &status.nickname {
        say!(
            format!(
                "Ваш ник: {nickname} — так вас видят друзья. Сменить: --nick <НИК>. Остановить: Ctrl+C."
            ),
            format!(
                "Your nickname: {nickname}. This is how friends see you. Change it with --nick <NICK>. Stop: Ctrl+C."
            )
        );
        println!();
    }
}

fn print_relays(relay: &RelayInfo) {
    if relay.loopback_only {
        return;
    }
    if !relay.custom {
        say!(
            "Relay: бесплатные серверы n0 (только для проверок). Свой relay: --relay <адрес>.",
            "Relay: free n0 servers (for testing only). Your own relay: --relay <address>."
        );
        return;
    }
    println!("Relay: {}.", relay.urls.join(", "));
}

fn print_host_ready(host: &HostReady) {
    println!();
    say!(
        "Приглашение для друга (скопируйте всю строку):",
        "Invite for a friend (copy the whole line):"
    );
    println!();
    println!("{}", host.ticket);
    println!();
    print_relays(&host.relay);
    if host.relay.needs_token {
        say!(
            "Relay требует пароль: передайте другу и его (--relay-token), приглашение пароль не содержит.",
            "The relay needs a password: give it to your friend too (--relay-token); the invite does not contain it."
        );
    }
    if host.relay.needs_ca {
        say!(
            "У relay свой сертификат: другу нужен тот же файл сертификата (--relay-ca).",
            "The relay has its own certificate: your friend needs the same certificate file (--relay-ca)."
        );
    }
    match host.game_port {
        Some(port) => say!(
            format!(
                "Мир «{}»: подключения друзей пойдут в игру на 127.0.0.1:{port}.",
                host.motd
            ),
            format!(
                "World \"{}\": friends' connections will go to the game on 127.0.0.1:{port}.",
                host.motd
            )
        ),
        None => say!(
            "Порт игры не указан (--port): работает только проверка канала (echo).",
            "No game port given (--port): only the connection check (echo) works."
        ),
    }
    if let Some(version) = &host.game_version {
        say!(
            format!(
                "Сервер сообщает версию: {version}. Обычно у друзей должна быть такая же версия Minecraft."
            ),
            format!(
                "The server reports version {version}. Friends usually need the same Minecraft version."
            )
        );
    }
    let voice = &host.voice;
    let ports: Vec<String> = voice.ports.iter().map(|p| p.port.to_string()).collect();
    match voice.mode {
        VoiceMode::Off => say!("Голос (UDP): выключен.", "Voice (UDP): off."),
        VoiceMode::Manual => say!(
            format!(
                "Голос (UDP): порты {} (заданы вручную; программа не проверяет, кто их держит — убедитесь, что это голосовой сервер).",
                ports.join(", ")
            ),
            format!(
                "Voice (UDP): ports {} (set manually; the program does not check who holds them, so make sure it is a voice server).",
                ports.join(", ")
            )
        ),
        VoiceMode::Auto => {
            if ports.is_empty() {
                say!(
                    "Голос (UDP): подходящих портов нет — голос не пересылается.",
                    "Voice (UDP): no suitable ports, so voice is not forwarded."
                );
            } else {
                say!(
                    format!("Голос (UDP): порты {}.", ports.join(", ")),
                    format!("Voice (UDP): ports {}.", ports.join(", "))
                );
            }
            for port in &voice.ports {
                println!("  • {}: {}", port.port, port.description);
            }
            for hint in &voice.hints {
                println!("  {hint}");
            }
            if !voice.hints.is_empty() {
                say!(
                    "  Параметры: --game-dir <папка игры>, --udp <порты>.",
                    "  Options: --game-dir <game folder>, --udp <ports>."
                );
            }
        }
    }
    say!(
        format!(
            "Разрешённых устройств: {}. Новые устройства: {}.",
            host.allowed_devices,
            if host.asks_new_devices {
                "спрошу вас"
            } else {
                "отклоняются"
            }
        ),
        format!(
            "Allowed devices: {}. New devices: {}.",
            host.allowed_devices,
            if host.asks_new_devices {
                "I will ask you"
            } else {
                "are declined"
            }
        )
    );
    if host.game_port.is_some() && host.look.settings.enabled {
        say!(
            format!(
                "Оформление в игре: шапка «{} • {} • онлайн» при входе и метка [HOST] в Tab \
                 (датапак в папке мира; применить сразу — /trigger phase.sync в игре).",
                host.look.title, host.look.code
            ),
            format!(
                "In-game style: a \"{} • {} • online\" header on join and a [HOST] tag in Tab \
                 (a datapack in the world folder; apply it now with /trigger phase.sync in game).",
                host.look.title, host.look.code
            )
        );
    }
}

fn print_guest_ready(guest: &GuestReady) {
    print_relays(&guest.relay);
    println!();
    match &guest.host_nickname {
        Some(host) => say!(
            format!("Готово! Вы в мире «{}» у игрока {host}.", guest.motd),
            format!("Done! You are in \"{}\", hosted by {host}.", guest.motd)
        ),
        None => say!(
            format!("Готово! Мир хоста: «{}».", guest.motd),
            format!("Done! The host's world: \"{}\".", guest.motd)
        ),
    }
    if let Some(version) = &guest.game_version {
        say!(
            format!(
                "Сервер хоста сообщает версию: {version}. Обычно нужна такая же версия Minecraft."
            ),
            format!(
                "The host's server reports version {version}. You usually need the same Minecraft version."
            )
        );
    }
    let local = &guest.local_addr;
    match &guest.lan_name {
        Some(name) => {
            say!(
                format!(
                    "В Minecraft: Сетевая игра → мир «{name}» появится в разделе «Локальная сеть» (через несколько секунд)."
                ),
                format!(
                    "In Minecraft: Multiplayer → the world \"{name}\" will show up as a LAN World in a few seconds."
                )
            );
            say!(
                format!("Если его там нет: Сетевая игра → Прямое подключение → {local}"),
                format!("If it is not there: Multiplayer → Direct Connection → {local}")
            );
        }
        None => say!(
            format!("В Minecraft: Сетевая игра → Прямое подключение → {local}"),
            format!("In Minecraft: Multiplayer → Direct Connection → {local}")
        ),
    }
    if !guest.voice_ports.is_empty() {
        let ports: Vec<String> = guest.voice_ports.iter().map(u16::to_string).collect();
        say!(
            format!(
                "Голос (UDP): открыты порты {} на 127.0.0.1.",
                ports.join(", ")
            ),
            format!(
                "Voice (UDP): ports {} are open on 127.0.0.1.",
                ports.join(", ")
            )
        );
    }
}

async fn run_echo(profile: &str, args: EchoArgs, status_every: Duration) -> Result<()> {
    let host_addr = args.ticket.endpoint_addr().clone();
    let net = args.net.options();
    let relay = net.relay_settings()?.with_relays_of(&host_addr);
    let network = net.mode;
    let dir = identity::profile_dir(profile)?;
    let key = identity::load_or_create_secret_key(&dir)?;
    let ep = endpoint::bind(EndpointOptions {
        secret_key: key,
        accept_incoming: false,
        network,
        relay: relay.clone(),
        access: None,
    })
    .await?;
    let nickname = phase_core::profiles::nickname(profile)?;
    say!(
        format!("Ваш ник: {nickname}. ID этого устройства: {}", ep.id()),
        format!("Your nickname: {nickname}. This device's ID: {}", ep.id())
    );
    if network != NetworkMode::LoopbackOnly {
        for warning in relay.warnings() {
            warn!("{warning}");
        }
    }
    let started = Instant::now();
    let (conn, info) = match guest::connect_once(&ep, &host_addr, Some(&nickname)).await {
        Ok(connected) => connected,
        Err(err) => {
            let err = with_relay_hint(err, &ep, &relay, network);
            ep.close().await;
            return Err(err);
        }
    };
    let label = format!(
        "{} {}",
        tr!("хост", "host"),
        wire::peer_label(&host_addr.id, info.nickname.as_deref())
    );
    info!(
        "соединение установлено за {:.0} мс",
        started.elapsed().as_secs_f64() * 1000.0
    );

    let telemetry = Telemetry::new();
    let monitor = metrics::spawn_monitor(conn.clone(), label, status_every, telemetry.clone());
    let interval = Duration::from_millis(args.interval_ms.max(1));
    say!(
        format!(
            "Замер: {} пакетов каждые {} мс (≈{:.0} с)…",
            args.count,
            interval.as_millis(),
            (interval * args.count).as_secs_f64()
        ),
        format!(
            "Probe: {} packets every {} ms (≈{:.0} s)…",
            args.count,
            interval.as_millis(),
            (interval * args.count).as_secs_f64()
        )
    );
    let report = match echo::run(&conn, args.count, interval).await {
        Ok(report) => report,
        Err(err) => {
            // The connection report is saved even on failure: it is needed for diagnostics.
            conn.close(0u32.into(), b"done");
            let _ = tokio::time::timeout(Duration::from_secs(2), monitor).await;
            ep.close().await;
            save_echo_report(
                &telemetry,
                serde_json::json!({ "error": format!("{err:#}") }),
            )?;
            return Err(err);
        }
    };

    println!();
    say!("Результат эхо-замера:", "Echo probe result:");
    say!(
        format!(
            "  получено ответов: {} из {} отправленных (без ответа: {})",
            report.received, report.sent, report.unanswered
        ),
        format!(
            "  replies received: {} of {} sent (unanswered: {})",
            report.received, report.sent, report.unanswered
        )
    );
    if let Some(err) = &report.error {
        say!(
            format!("  ЗАМЕР ПРЕРВАН: {err}"),
            format!("  PROBE INTERRUPTED: {err}")
        );
    } else if report.timed_out {
        say!(
            "  часть ответов не пришла за отведённое время",
            "  some replies did not arrive in time"
        );
    }
    say!(
        format!(
            "  пинг через туннель: медиана {:.1} мс · 95% {:.1} мс · 99% {:.1} мс · макс {:.1} мс",
            report.p50_ms, report.p95_ms, report.p99_ms, report.max_ms
        ),
        format!(
            "  ping through the tunnel: median {:.1} ms · 95% {:.1} ms · 99% {:.1} ms · max {:.1} ms",
            report.p50_ms, report.p95_ms, report.p99_ms, report.max_ms
        )
    );
    say!(
        format!(
            "  самая длинная пауза между ответами: {:.0} мс",
            report.max_gap_ms
        ),
        format!("  longest gap between replies: {:.0} ms", report.max_gap_ms)
    );
    if let (Some(kind), Some(rtt)) = (report.path_kind, report.path_rtt_ms) {
        say!(
            format!("  путь: {} · пинг пути (QUIC): {rtt:.1} мс", kind.label()),
            format!("  path: {} · path ping (QUIC): {rtt:.1} ms", kind.label())
        );
        // QUIC's path ping is smoothed, so the difference can be negative:
        // that means our delay is below the measurement precision.
        let added = report.p50_ms - rtt;
        if added < 1.0 {
            say!(
                "  добавлено программой: меньше 1 мс (в пределах точности замера)",
                "  added by the program: under 1 ms (within the probe's precision)"
            );
        } else {
            say!(
                format!("  добавлено программой (медиана эха − пинг пути): {added:.1} мс"),
                format!("  added by the program (echo median − path ping): {added:.1} ms")
            );
        }
    }

    conn.close(0u32.into(), b"done");
    let _ = tokio::time::timeout(Duration::from_secs(2), monitor).await;
    ep.close().await;
    save_echo_report(&telemetry, serde_json::to_value(&report)?)?;
    Ok(())
}

fn save_echo_report(telemetry: &Telemetry, extra: serde_json::Value) -> Result<()> {
    if let Some(path) = reports::save(&reports_dir(), "echo", telemetry, Some(extra))? {
        say!(
            format!("Отчёт сохранён: {}", path.display()),
            format!("Report saved: {}", path.display())
        );
    }
    Ok(())
}

/// Asks in the console whether to let a new device in.
///
/// A separate thread reads keyboard lines. It does not block the program from exiting:
/// Ctrl+C does not hang on unfinished input. Before each question, old lines
/// typed earlier are discarded so stray input does not become an answer.
#[derive(Debug)]
struct ConsoleApprover {
    lines: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>>,
}

impl ConsoleApprover {
    fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("stdin".into())
            .spawn(move || {
                let mut line = String::new();
                loop {
                    line.clear();
                    match std::io::stdin().read_line(&mut line) {
                        Ok(0) | Err(_) => break, // input closed
                        Ok(_) => {
                            if tx.send(line.trim().to_string()).is_err() {
                                break;
                            }
                        }
                    }
                }
            })
            .expect("failed to start reading the keyboard");
        Self {
            lines: Arc::new(tokio::sync::Mutex::new(rx)),
        }
    }
}

impl Approver for ConsoleApprover {
    fn approve(&self, id: EndpointId, nickname: Option<String>) -> BoxFuture<bool> {
        let lines = self.lines.clone();
        Box::pin(async move {
            let mut lines = lines.lock().await;
            while lines.try_recv().is_ok() {} // old input does not count as an answer
            println!();
            match &nickname {
                Some(nickname) => say!(
                    format!("Игрок {nickname} хочет войти в комнату."),
                    format!("{nickname} wants to join the room.")
                ),
                None => say!(
                    "Новое устройство хочет войти в комнату (ник не назван).",
                    "A new device wants to join the room (no nickname given)."
                ),
            }
            say!(format!("ID устройства: {id}"), format!("Device ID: {id}"));
            say!(
                "Ник игрок называет сам, а ID подделать нельзя: сверьте ID с тем, что напечатал друг. \
                 Пустить? Введите y (да) или n (нет) и нажмите Enter:",
                "The player picks the nickname themselves, but the ID cannot be faked: compare it with what your friend sees. \
                 Let them in? Type y (yes) or n (no) and press Enter:"
            );
            match lines.recv().await {
                Some(answer) => matches!(answer.to_lowercase().as_str(), "y" | "yes" | "д" | "да"),
                None => {
                    warn!(
                        "{}",
                        tr!(
                            "ввод с клавиатуры недоступен — устройство отклонено",
                            "keyboard input is unavailable: the device was declined"
                        )
                    );
                    false
                }
            }
        })
    }
}
