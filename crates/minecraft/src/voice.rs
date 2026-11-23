//! Search for UDP ports of voice mods (Simple Voice Chat, Plasmo Voice) on the host PC.
//!
//! How the mods choose a port (checked against their source code):
//! - **Simple Voice Chat** in a LAN world (integrated server) takes a **random** free
//!   port; on a dedicated server, the port from its settings (24454 by default) or the game port.
//!   It writes `Voice chat server started at port N` (or `… at <address>:N`) to the log.
//! - **Plasmo Voice** by default takes the Minecraft server port (setting `host.port = 0`).
//!   It writes `… UDP server is started on /0.0.0.0:N` to the log.
//!
//! Guests will be able to send UDP packets to the announced ports on the host's `127.0.0.1`, so
//! the trust rules are strict:
//! 1. The game process is the one that listens on the world port, **and only if a Minecraft server
//!    answered on the port** (status query). Otherwise the game process is considered unknown.
//! 2. A port from the log (`logs\latest.log`) is taken only if the game process holds it.
//!    So neither old entries from past launches nor forgery through chat work
//!    (any chat line can be written, but another program's port does not
//!    belong to the game process).
//! 3. Other UDP ports of the game process only if the log gave nothing (the launcher keeps
//!    the log in an unknown place): otherwise these are extra service sockets of the game.
//! 4. Fallback ports (the game port and 24454) always stay, **except** when the port
//!    is taken by another program: we do not forward guests' packets to someone else's program.

use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use crate::lan::LAN_PORT;

/// Default Simple Voice Chat port on a dedicated server.
pub const SVC_DEFAULT_PORT: u16 = 24454;
/// How many ports to announce to the guest (the guest will not open more anyway).
pub const MAX_PORTS: usize = 8;
/// Lower numbers are system ports; the guest does not open them (see the tunnel, `voice.rs`).
const MIN_PORT: u16 = 1024;
/// We do not read logs from known folders older than this: a running game writes to its log
/// constantly. The limit does not apply to folders given by the user (`--game-dir`).
const LOG_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
/// We read only the end of a large log.
const LOG_TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// Voice mod.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceMod {
    SimpleVoiceChat,
    PlasmoVoice,
}

impl VoiceMod {
    pub fn name(self) -> &'static str {
        match self {
            Self::SimpleVoiceChat => "Simple Voice Chat",
            Self::PlasmoVoice => "Plasmo Voice",
        }
    }
}

/// Where the port is known from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortSource {
    /// A voice server launch line in the log, and the game process currently holds the port.
    Log { voice_mod: VoiceMod, file: PathBuf },
    /// A fallback port currently held by the game process (for example, Plasmo Voice on the world
    /// port).
    GameProcess,
    /// Another UDP port of the game process: the mod is not identified by the log, but the port belongs to the game.
    UnidentifiedGameSocket,
    /// A fallback port that nobody has open right now (useful if the mod starts later).
    Fallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoicePort {
    pub port: u16,
    pub source: PortSource,
}

/// Search result.
#[derive(Debug, Clone, Default)]
pub struct VoiceDetection {
    /// Ports in descending order of confidence, at most [`MAX_PORTS`].
    pub ports: Vec<VoicePort>,
    /// The game process (`None` means not found or the world port was not confirmed by a status query).
    pub game_pid: Option<u32>,
    /// Why the game process was not identified (`None` if it was).
    pub game_unconfirmed: Option<String>,
    /// Which logs were read.
    pub logs_read: Vec<PathBuf>,
    /// Ports that are **not** announced, and why.
    pub skipped: Vec<(u16, String)>,
}

impl VoiceDetection {
    pub fn port_numbers(&self) -> Vec<u16> {
        self.ports.iter().map(|p| p.port).collect()
    }
}

/// Who holds a UDP port (according to a port table snapshot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortOwnership {
    /// Nobody holds the port.
    Free,
    /// Only the game process holds the port.
    Game,
    /// Other processes hold the port (possibly along with the game).
    Others(Vec<u32>),
    /// The port table could not be read.
    Unknown(String),
}

/// Port classification from a table snapshot. A port is "the game's" only if **all** its sockets
/// belong to the game: if there is a foreign socket on `127.0.0.1:P` next to it, it will receive the packets.
pub fn ownership(
    table: &Result<Vec<phase_netstat::PortOwner>, String>,
    port: u16,
    game_pid: Option<u32>,
) -> PortOwnership {
    let table = match table {
        Ok(table) => table,
        Err(err) => return PortOwnership::Unknown(err.clone()),
    };
    let owners = phase_netstat::owners_of(table, port);
    match (owners.as_slice(), game_pid) {
        ([], _) => PortOwnership::Free,
        ([only], Some(game)) if *only == game => PortOwnership::Game,
        _ => PortOwnership::Others(
            owners
                .into_iter()
                .filter(|p| Some(*p) != game_pid)
                .collect(),
        ),
    }
}

/// Looks for voice ports for the world on port `game_port`.
///
/// - `game_verified`: a Minecraft server answered on the world port (status query). Without it
///   the process listening on the port is not considered the game, and only free fallback ports remain.
/// - `logs`: game logs (see [`candidate_logs`]);
/// - `fallback`: fallback ports (usually the game port and 24454).
pub fn detect(
    game_port: u16,
    game_verified: bool,
    logs: &[PathBuf],
    fallback: &[u16],
) -> VoiceDetection {
    let (game_pid, game_unconfirmed) = if !game_verified {
        (
            None,
            Some(
                phase_lang::tr!(
                    "сервер Minecraft не ответил на запрос статуса",
                    "the Minecraft server did not answer the status request"
                )
                .to_string(),
            ),
        )
    } else {
        match phase_netstat::tcp_listener_owner(game_port) {
            Ok(Some(pid)) => (Some(pid), None),
            Ok(None) => (
                None,
                Some(phase_lang::tr!(
                    format!(
                        "порт {game_port} слушает не один процесс (или уже никто) — непонятно, какой из них игра"
                    ),
                    format!(
                        "port {game_port} is not held by a single process (or by none anymore): can't tell which one is the game"
                    )
                )),
            ),
            Err(err) => (
                None,
                Some(phase_lang::tr!(
                    format!("не удалось прочитать таблицу портов: {err}"),
                    format!("could not read the port table: {err}")
                )),
            ),
        }
    };
    // One snapshot of the UDP port table for the whole search.
    let table = phase_netstat::udp_sockets().map_err(|e| e.to_string());
    let game_ports: Vec<u16> = match (&table, game_pid) {
        (Ok(table), Some(pid)) => {
            let mut ports: Vec<u16> = table
                .iter()
                .filter(|s| s.pid == pid)
                .map(|s| s.port)
                .filter(|&p| p != LAN_PORT) // 4445 is the game's own LAN announcement port, not voice
                .collect();
            ports.sort_unstable();
            ports.dedup();
            ports
        }
        _ => Vec::new(),
    };

    let mut result = VoiceDetection {
        game_pid,
        game_unconfirmed: game_unconfirmed.clone(),
        ..VoiceDetection::default()
    };
    let mut candidates: Vec<VoicePort> = Vec::new();
    // Ports below 1024 are system ports: the guest does not open them, no point announcing.
    let mut add = |port: u16, source: PortSource| {
        if port >= MIN_PORT && port != LAN_PORT && !candidates.iter().any(|p| p.port == port) {
            candidates.push(VoicePort { port, source });
        }
    };
    // Why the port does not fit (if it does not). The same explanation only once.
    let skip = |skipped: &mut Vec<(u16, String)>, port: u16, why: String| {
        if !skipped.iter().any(|(p, _)| *p == port) {
            skipped.push((port, why));
        }
    };
    let refusal = |ownership: &PortOwnership, port: u16| -> Option<String> {
        match ownership {
            PortOwnership::Free | PortOwnership::Game => None,
            PortOwnership::Others(pids) => {
                let pids = pids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let why = match &game_unconfirmed {
                    None => phase_lang::tr!(
                        format!("его держит другая программа (номер процесса {pids})"),
                        format!("it is held by another program (process ID {pids})")
                    ),
                    Some(reason) => phase_lang::tr!(
                        format!(
                            "его держит процесс {pids}, а игра не определена ({reason}) — \
                             неизвестной программе пакеты гостей не пересылаются"
                        ),
                        format!(
                            "it is held by process {pids} and the game could not be identified ({reason}): \
                             guest packets are not forwarded to an unknown program"
                        )
                    ),
                };
                Some(phase_lang::tr!(
                    format!(
                        "{why}. Если это ваш голосовой сервер, укажите порт вручную: --udp {port}"
                    ),
                    format!(
                        "{why}. If this is your voice server, set the port manually: --udp {port}"
                    )
                ))
            }
            PortOwnership::Unknown(err) => Some(phase_lang::tr!(
                format!("не удалось проверить, какая программа держит порт: {err}"),
                format!("could not check which program holds the port: {err}")
            )),
        }
    };

    // 1. Logs, only if the game process is known (otherwise there is nothing to confirm the port with).
    let mut from_log = 0;
    if game_pid.is_some() {
        for file in logs {
            let Ok(found) = scan_log(file) else { continue };
            result.logs_read.push(file.clone());
            for (voice_mod, port) in found {
                let owner = ownership(&table, port, game_pid);
                match owner {
                    PortOwnership::Game => {
                        from_log += 1;
                        add(
                            port,
                            PortSource::Log {
                                voice_mod,
                                file: file.clone(),
                            },
                        );
                    }
                    // A port from an old log entry, closed now: report nothing.
                    PortOwnership::Free => {}
                    _ => {
                        if let Some(why) = refusal(&owner, port) {
                            skip(
                                &mut result.skipped,
                                port,
                                phase_lang::tr!(
                                    format!("{} в журнале: {why}", voice_mod.name()),
                                    format!("{} in the log: {why}", voice_mod.name())
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback ports, if no foreign (or unidentified) program holds them.
    for &port in fallback {
        let owner = ownership(&table, port, game_pid);
        match &owner {
            PortOwnership::Free => add(port, PortSource::Fallback),
            PortOwnership::Game => add(port, PortSource::GameProcess),
            _ => {
                if let Some(why) = refusal(&owner, port) {
                    skip(&mut result.skipped, port, why);
                }
            }
        }
    }

    // 3. Other ports of the game process, only if the log gave nothing.
    if from_log == 0 {
        for &port in &game_ports {
            let owner = ownership(&table, port, game_pid);
            if owner == PortOwnership::Game {
                add(port, PortSource::UnidentifiedGameSocket);
            } else if let Some(why) = refusal(&owner, port) {
                skip(&mut result.skipped, port, why);
            }
        }
    }

    if candidates.len() > MAX_PORTS {
        for extra in candidates.drain(MAX_PORTS..) {
            skip(
                &mut result.skipped,
                extra.port,
                phase_lang::tr!(
                    format!("не помещается: гостю объявляется не больше {MAX_PORTS} портов"),
                    format!("does not fit: at most {MAX_PORTS} ports are announced to a guest")
                ),
            );
        }
    }
    // A port that ends up announced is not listed among the skipped ones.
    result
        .skipped
        .retain(|(p, _)| !candidates.iter().any(|c| c.port == *p));
    result.ports = candidates;
    result
}
/// Logs that may contain voice mod lines: `<game folder>\logs\latest.log`.
///
/// First the user's folders (`--game-dir`), then the known launcher folders on this PC
/// (see [`known_game_dirs`]).
pub fn candidate_logs(user_dirs: &[PathBuf]) -> Vec<PathBuf> {
    latest_logs(user_dirs, &known_game_dirs())
}

/// Known game folders of different launchers (only those that exist on this PC, checked in
/// [`latest_logs`]).
pub fn known_game_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        // The official launcher (and others that use the standard folder).
        dirs.push(appdata.join(".minecraft"));
        // Feather Client.
        dirs.push(appdata.join(".minecraft").join("feather"));
        // Legacy Launcher.
        dirs.push(
            appdata
                .join(".tlauncher")
                .join("legacy")
                .join("Minecraft")
                .join("game"),
        );
        // Prism Launcher: each instance has its own game folder.
        for instance in subdirs(&appdata.join("PrismLauncher").join("instances")) {
            dirs.push(instance.join("minecraft"));
            dirs.push(instance.join(".minecraft"));
        }
        // Modrinth App: the profile folder is the game folder.
        for app in ["ModrinthApp", "com.modrinth.theseus"] {
            dirs.extend(subdirs(&appdata.join(app).join("profiles")));
        }
    }
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        // Lunar Client: a separate folder per version.
        dirs.extend(subdirs(&profile.join(".lunarclient").join("profiles")));
    }
    dirs
}

/// Existing `logs\latest.log` in the folders: first from `user_dirs` (in the given order,
/// with no age limit), then from `known_dirs`, fresh ones only, newest first.
pub fn latest_logs(user_dirs: &[PathBuf], known_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let log_of = |dir: &PathBuf| dir.join("logs").join("latest.log");
    let mut out: Vec<PathBuf> = user_dirs
        .iter()
        .map(log_of)
        .filter(|file| file.is_file())
        .collect();

    let now = SystemTime::now();
    let mut known: Vec<(SystemTime, PathBuf)> = known_dirs
        .iter()
        .map(log_of)
        .filter_map(|file| {
            let modified = fs::metadata(&file).and_then(|m| m.modified()).ok()?;
            let fresh = now.duration_since(modified).unwrap_or_default() < LOG_MAX_AGE;
            fresh.then_some((modified, file))
        })
        .collect();
    known.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified)); // newest first
    out.extend(known.into_iter().map(|(_, file)| file));

    let mut seen = HashSet::new();
    out.retain(|file| seen.insert(file.clone()));
    out
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// All voice server launch lines in the log: (mod, port), in order.
pub fn scan_log(path: &Path) -> io::Result<Vec<(VoiceMod, u16)>> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len > LOG_TAIL_BYTES {
        file.seek(SeekFrom::Start(len - LOG_TAIL_BYTES))?;
    }
    let mut raw = Vec::new();
    file.take(LOG_TAIL_BYTES).read_to_end(&mut raw)?;
    Ok(String::from_utf8_lossy(&raw)
        .lines()
        .filter_map(parse_log_line)
        .collect())
}

const SVC_PHRASE: &str = "Voice chat server started at";
const PLASMO_PHRASE: &str = "UDP server is started on";
const PLASMO_REUSEPORT_PHRASE: &str = "UDP server instances with SO_REUSEPORT on";

/// A log line about a voice server launch → (mod, port).
///
/// The mod's phrase must be **at the very start of the message**, right after the header
/// `[time] [thread/LEVEL]: `. Player messages do not look like that: chat starts with
/// `<Nick>`, `/me` with `* Nick`, `/say` with `[Nick]`, chat in the client log with `[CHAT]`.
/// The main protection is still in [`detect`]: the port must belong to the game process.
pub fn parse_log_line(line: &str) -> Option<(VoiceMod, u16)> {
    // The message comes after the header `[time] [thread/LEVEL]: `. Lines without such a
    // header (continuations of multi-line entries, output of other programs) are not accepted.
    let header_end = line.find("]: ")?;
    if !line.starts_with('[') || !line[..header_end].contains('/') {
        return None;
    }
    let message = &line[header_end + 3..];
    // A mod tag before the message in some builds (`[voicechat] …`) is allowed.
    // It could only be forged by a player named voicechat via /say, and even then the port
    // passes only if it belongs to the game itself (checked in `detect`).
    let svc_message = message.strip_prefix("[voicechat] ").unwrap_or(message);
    let voice_mod = if svc_message.starts_with(SVC_PHRASE) {
        VoiceMod::SimpleVoiceChat
    } else if is_plasmo_start(message) {
        VoiceMod::PlasmoVoice
    } else {
        return None;
    };
    Some((voice_mod, trailing_port(message)?))
}

/// `<Channel> UDP server is started on …` or
/// `Bound <N> <Channel> UDP server instances with SO_REUSEPORT on …`,
/// where the channel is a Netty class name (`NioDatagramChannel`, `EpollDatagramChannel`…).
fn is_plasmo_start(message: &str) -> bool {
    let is_channel =
        |word: &str| word.ends_with("DatagramChannel") && word.chars().all(char::is_alphanumeric);
    let mut words = message.split(' ');
    match words.next() {
        Some("Bound") => {
            let count = words.next().unwrap_or_default();
            let channel = words.next().unwrap_or_default();
            !count.is_empty()
                && count.chars().all(|c| c.is_ascii_digit())
                && is_channel(channel)
                && message.contains(PLASMO_REUSEPORT_PHRASE)
        }
        Some(channel) if is_channel(channel) => message[channel.len()..]
            .trim_start()
            .starts_with(PLASMO_PHRASE),
        _ => false,
    }
}
/// Port number at the end of the line: `… port 24454`, `… 0.0.0.0:24454`, `… /[::]:24454`.
fn trailing_port(line: &str) -> Option<u16> {
    let line = line.trim_end();
    let digits_start = line
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_digit())
        .last()?
        .0;
    let before = line[..digits_start].chars().next_back()?;
    if before != ':' && before != ' ' {
        return None;
    }
    line[digits_start..].parse().ok().filter(|&p| p != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_voice_chat_lines() {
        assert_eq!(
            parse_log_line(
                "[12:01:02] [VoiceChatServerThread/INFO]: Voice chat server started at port 54321"
            ),
            Some((VoiceMod::SimpleVoiceChat, 54321))
        );
        assert_eq!(
            parse_log_line(
                "[12:01:02] [VoiceChatServerThread/INFO]: [voicechat] Voice chat server started at port 54322"
            ),
            Some((VoiceMod::SimpleVoiceChat, 54322))
        );
        assert_eq!(
            parse_log_line("[Server thread/INFO]: Voice chat server started at 0.0.0.0:24454\r"),
            Some((VoiceMod::SimpleVoiceChat, 24454))
        );
    }

    #[test]
    fn parses_plasmo_voice_lines() {
        assert_eq!(
            parse_log_line(
                "[12:00:00] [Server thread/INFO]: NioDatagramChannel UDP server is started on /0.0.0.0:61234"
            ),
            Some((VoiceMod::PlasmoVoice, 61234))
        );
        assert_eq!(
            parse_log_line(
                "[12:00:00] [Server thread/INFO]: Bound 4 EpollDatagramChannel UDP server instances with SO_REUSEPORT on /[0:0:0:0:0:0:0:0]:25565"
            ),
            Some((VoiceMod::PlasmoVoice, 25565))
        );
    }

    #[test]
    fn ignores_unrelated_and_broken_lines() {
        assert_eq!(parse_log_line("[INFO]: Started serving on 51234"), None);
        assert_eq!(parse_log_line("Voice chat server started at port"), None);
        assert_eq!(
            parse_log_line("Voice chat server started at port 99999"),
            None
        );
        assert_eq!(parse_log_line("Voice chat server started at port 0"), None);
        assert_eq!(
            parse_log_line("Voice chat server started at port x12"),
            None
        );
        assert_eq!(parse_log_line("UDP server is stopped"), None);
        assert_eq!(
            parse_log_line(
                "[INFO]: 4 EpollDatagramChannel UDP server instances with SO_REUSEPORT on /0.0.0.0:1"
            ),
            None,
            "without \"Bound\" this is not a Plasmo Voice line"
        );
    }

    #[test]
    fn ignores_chat_messages_and_commands() {
        for line in [
            // Chat on a server.
            "[12:00:00] [Server thread/INFO]: <Guest> Voice chat server started at port 1900",
            // /me is allowed for any player.
            "[12:00:00] [Server thread/INFO]: * Guest Voice chat server started at port 1900",
            "[12:00:00] [Server thread/INFO]: * Guest NioDatagramChannel UDP server is started on /0.0.0.0:1900",
            // /say.
            "[12:00:00] [Server thread/INFO]: [Guest] Voice chat server started at port 1900",
            // Chat in the client log.
            "[12:00:00] [Render thread/INFO]: [System] [CHAT] NioDatagramChannel UDP server is started on /0.0.0.0:5353",
            "[12:00:00] [Render thread/INFO]: [System] [CHAT] <Guest> Voice chat server started at port 5353",
            // Output of another program via STDOUT.
            "[12:00:00] [Render thread/INFO]: [STDOUT]: Voice chat server started at port 5353",
        ] {
            assert_eq!(parse_log_line(line), None, "{line}");
        }
    }

    fn table(rows: &[(u16, u32)]) -> Result<Vec<phase_netstat::PortOwner>, String> {
        Ok(rows
            .iter()
            .map(|&(port, pid)| phase_netstat::PortOwner { port, pid })
            .collect())
    }

    #[test]
    fn port_belongs_to_game_only_if_every_socket_is_the_games() {
        let game = Some(100);
        let t = table(&[
            (5000, 100),
            (5000, 100),
            (6000, 100),
            (6000, 200),
            (7000, 200),
        ]);
        assert_eq!(ownership(&t, 5000, game), PortOwnership::Game);
        // The game on 0.0.0.0:6000, another program on 127.0.0.1:6000: the port is not the game's.
        assert_eq!(ownership(&t, 6000, game), PortOwnership::Others(vec![200]));
        assert_eq!(ownership(&t, 7000, game), PortOwnership::Others(vec![200]));
        assert_eq!(ownership(&t, 8000, game), PortOwnership::Free);
        // The game is not confirmed: any owner is considered foreign.
        assert_eq!(ownership(&t, 5000, None), PortOwnership::Others(vec![100]));
        // The table could not be read: we assert nothing.
        let broken = Err("no access".to_string());
        assert!(matches!(
            ownership(&broken, 5000, game),
            PortOwnership::Unknown(_)
        ));
    }
    #[test]
    fn scans_log_file() {
        let dir = std::env::temp_dir().join(format!("phase-test-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("latest.log");
        fs::write(
            &file,
            "[10:00:00] [main/INFO]: Loading\n\
             [10:00:01] [Server thread/INFO]: Voice chat server started at port 50001\n\
             [10:00:02] [Server thread/INFO]: something\n\
             Voice chat server started at port 50003\n\
             [10:00:03] [Server thread/INFO]: NioDatagramChannel UDP server is started on /0.0.0.0:50002\n",
        )
        .unwrap();
        assert_eq!(
            scan_log(&file).unwrap(),
            vec![
                (VoiceMod::SimpleVoiceChat, 50001),
                (VoiceMod::PlasmoVoice, 50002)
            ]
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
