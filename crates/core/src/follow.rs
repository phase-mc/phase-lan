//! The room follows the host's world.
//!
//! The host left the world ("Save and quit") and opened it to the network again: the game takes a
//! **new** port. Without this the room would stay on a dead port: the code and the invitation
//! would lead nowhere, and the styling in the game would switch off.
//!
//! While the room runs, [`watch`] checks every few seconds whether anything is listening on
//! the game port. If the port went silent, we look for worlds open to the network on this PC (ports of game
//! processes that answered as a Minecraft server) and decide ([`decide`]) which of them is the
//! room's world:
//!
//! 1. a world with the same folder as the room's (`saves/<world>`);
//! 2. otherwise, a world of the same game (the same process) that held the old port: the host opened
//!    a different world in their game;
//! 3. otherwise, the only open world, if the room's game is gone (it was restarted).
//!
//! A world of someone else's game while the room's game is running, and several suitable worlds, are not our
//! case: the room waits and logs why.
//!
//! If the port was found by the program (`auto`, auto room), the room moves to the new port by itself:
//! the same code, the same guests, the guest's next game connection goes into the new world.
//! If the port was set manually, the room stays on it and the host gets a hint.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

use phase_lang::tr;
use phase_minecraft::{lan, status, world};
use phase_tunnel::{host::GameTarget, wire};
use tokio::time::Instant;

use crate::{events::SessionEvent, look::Look, session::Ctx};

/// How often to check the game port.
const CHECK_EVERY: Duration = Duration::from_secs(3);
/// A game process port that did not answer as Minecraft is not rechecked sooner than this.
const NOT_A_WORLD_FOR: Duration = Duration::from_secs(20);

/// What is known about the room's world.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RoomGame {
    /// The room's world folder, if the world was found.
    pub world: Option<PathBuf>,
    /// The game process that held the room's port.
    pub pid: Option<u32>,
}

/// A world open to the network on this PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub port: u16,
    pub pid: u32,
    /// World folder (`None` means it was not determined).
    pub world: Option<PathBuf>,
    /// World name (from the server's reply).
    pub name: String,
    pub summary: String,
}

/// Decision about the room's port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// No open worlds: wait.
    Wait,
    /// The room's world is this one (an index into the candidate list).
    Switch(usize),
    /// There are worlds, but it is unclear which is the room's. The port is not changed; the reason goes to the log.
    Unclear(String),
}

/// Which of the open worlds is the room's world. `games` are the running game processes.
pub(crate) fn decide(room: &RoomGame, candidates: &[Candidate], games: &[u32]) -> Decision {
    if candidates.is_empty() {
        return Decision::Wait;
    }
    let ports = |found: &[usize]| {
        found
            .iter()
            .map(|&at| candidates[at].port.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let matching = |test: &dyn Fn(&Candidate) -> bool| -> Vec<usize> {
        (0..candidates.len())
            .filter(|&at| test(&candidates[at]))
            .collect()
    };
    // 1. The same world (the same folder).
    if let Some(dir) = &room.world {
        match matching(&|c| c.world.as_ref() == Some(dir)).as_slice() {
            [] => {}
            [one] => return Decision::Switch(*one),
            many => {
                return Decision::Unclear(tr!(
                    format!(
                        "мир комнаты открыт сразу на нескольких портах ({})",
                        ports(many)
                    ),
                    format!(
                        "the room's world is open on several ports at once ({})",
                        ports(many)
                    )
                ));
            }
        }
    }
    // 2. The same game: the host opened a world in it anew (or a different world).
    if let Some(pid) = room.pid {
        match matching(&|c| c.pid == pid).as_slice() {
            [] => {}
            [one] => return Decision::Switch(*one),
            many => {
                return Decision::Unclear(tr!(
                    format!(
                        "игра комнаты открыла для сети несколько миров (порты {})",
                        ports(many)
                    ),
                    format!(
                        "the room's game opened several worlds to LAN (ports {})",
                        ports(many)
                    )
                ));
            }
        }
        if games.contains(&pid) {
            let all: Vec<usize> = (0..candidates.len()).collect();
            return Decision::Unclear(tr!(
                format!(
                    "для сети открыт мир другой игры (порт {}), а игра комнаты ещё запущена — \
                     комната ждёт свой мир",
                    ports(&all)
                ),
                format!(
                    "another game has a world open to LAN (port {}) while the room's game is still running — \
                     the room is waiting for its own world",
                    ports(&all)
                )
            ));
        }
    }
    // 3. The room's game is gone (or unknown): take a world only if there is exactly one.
    match candidates {
        [_] => Decision::Switch(0),
        _ => {
            let all: Vec<usize> = (0..candidates.len()).collect();
            Decision::Unclear(tr!(
                format!(
                    "для сети открыто несколько миров (порты {}) — непонятно, какой из них комнаты",
                    ports(&all)
                ),
                format!(
                    "several worlds are open to LAN (ports {}) — can't tell which one is the room's",
                    ports(&all)
                )
            ))
        }
    }
}

/// What the follower needs.
pub(crate) struct Follow {
    /// The port was found by the program: the room moves to the new port by itself. `false` means the port
    /// was set manually, hint only.
    pub follow: bool,
    pub target: GameTarget,
    pub look: Look,
    pub ctx: Ctx,
}

/// Watches the game port while the room runs (the task is aborted when it closes).
pub(crate) async fn watch(follow: Follow) {
    let Follow {
        follow,
        target,
        look,
        ctx,
    } = follow;
    let Some(start) = target.get() else {
        return;
    };
    let owners = listeners(start).await;
    // The world was silent already at startup: the host was warned about it, we do not write it a second time.
    let mut closed = owners.as_ref().is_some_and(Vec::is_empty);
    let mut pid = owners.and_then(single);
    let mut hinted: Option<u16> = None;
    let mut unclear: Option<String> = None;
    let mut not_worlds: HashMap<u16, Instant> = HashMap::new();
    loop {
        tokio::time::sleep(CHECK_EVERY).await;
        let Some(port) = target.get() else {
            return;
        };
        let owners = listeners(port).await;
        let alive = match &owners {
            Some(owners) if !owners.is_empty() => true,
            // The port table is empty or unreadable: ask the port itself.
            _ => answers(port).await,
        };
        if alive {
            if let Some(owner) = owners.and_then(single) {
                pid = Some(owner);
            }
            if closed {
                closed = false;
                tracing::info!("мир на порту {port} снова отвечает");
                // The window showed a hint about another port: the world is back on the set one.
                if hinted.is_some() {
                    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
                    let checked = status::query(addr, status::DEFAULT_TIMEOUT).await;
                    ctx.bus.send(SessionEvent::GameChecked {
                        port,
                        summary: checked.as_ref().ok().map(status::ServerStatus::summary),
                        error: checked.err().map(|err| format!("{err:#}")),
                    });
                }
            }
            (hinted, unclear) = (None, None);
            continue;
        }
        if !closed {
            closed = true;
            ctx.info(if follow {
                tr!(
                    format!(
                        "Мир на порту {port} закрыт. Комната ждёт: откройте мир для сети снова — \
                         друзья войдут по тому же коду."
                    ),
                    format!(
                        "The world on port {port} is closed. The room is waiting: open the world to LAN again — \
                         friends will join with the same code."
                    )
                )
            } else {
                tr!(
                    format!("Мир на порту {port} закрыт или не отвечает."),
                    format!("The world on port {port} is closed or not responding.")
                )
            });
        }
        let (candidates, games) = scan(port, &mut not_worlds).await;
        let room = RoomGame {
            world: look.world_dir(),
            pid,
        };
        match decide(&room, &candidates, &games) {
            Decision::Wait => {}
            Decision::Unclear(why) => {
                if unclear.as_deref() != Some(why.as_str()) {
                    ctx.warn(tr!(
                        format!("Комната остаётся на порту {port}: {why}."),
                        format!("The room stays on port {port}: {why}.")
                    ));
                    unclear = Some(why);
                }
            }
            Decision::Switch(at) => {
                let found = &candidates[at];
                if follow {
                    // New guest game connections will now go here.
                    target.set(found.port);
                    look.set_game_port(found.port, Some(found.name.clone()));
                    ctx.update(|s| {
                        if let Some(host) = &mut s.host {
                            host.game_port = Some(found.port);
                        }
                    });
                    tracing::info!(
                        "мир «{}» снова открыт для сети на порту {} (был {port}): комната перешла туда",
                        found.name,
                        found.port
                    );
                    pid = Some(found.pid);
                    (closed, unclear) = (false, None);
                } else if hinted != Some(found.port) {
                    // The port was set manually: we do not change it, only hint (once).
                    tracing::info!(
                        "мир «{}» открыт на порту {}, а комната ведёт на заданный вручную {port}",
                        found.name,
                        found.port
                    );
                    hinted = Some(found.port);
                } else {
                    continue;
                }
                ctx.bus.send(SessionEvent::WorldMoved {
                    from: port,
                    to: found.port,
                    name: found.name.clone(),
                    summary: found.summary.clone(),
                    followed: follow,
                });
            }
        }
    }
}

/// The only process in the list.
fn single(owners: Vec<u32>) -> Option<u32> {
    match owners.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// Who listens on the port (`None` means the table cannot be read).
async fn listeners(port: u16) -> Option<Vec<u32>> {
    tokio::task::spawn_blocking(move || lan::port_listeners(port).ok())
        .await
        .ok()
        .flatten()
}

/// Whether a Minecraft server answers on the port.
async fn answers(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    status::query(addr, status::DEFAULT_TIMEOUT).await.is_ok()
}

/// Worlds open to the network on this PC (except port `skip`), and the running game processes.
async fn scan(skip: u16, not_worlds: &mut HashMap<u16, Instant>) -> (Vec<Candidate>, Vec<u32>) {
    let ports = tokio::task::spawn_blocking(lan::game_ports)
        .await
        .unwrap_or_default();
    let now = Instant::now();
    not_worlds.retain(|_, until| *until > now);
    let mut answered = Vec::new();
    for &(port, pid) in &ports.listening {
        if port == skip || not_worlds.contains_key(&port) {
            continue;
        }
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        match status::query(addr, status::DEFAULT_TIMEOUT).await {
            Ok(found) => answered.push((port, pid, found)),
            Err(err) => {
                tracing::debug!("порт {port} процесса игры не ответил как Minecraft: {err:#}");
                not_worlds.insert(port, now + NOT_A_WORLD_FOR);
            }
        }
    }
    // World folders: the search blocks (process tables, session.lock locks).
    let candidates = tokio::task::spawn_blocking(move || {
        answered
            .into_iter()
            .map(|(port, pid, found)| {
                let name = wire::sanitize_text(&found.motd, wire::MOTD_MAX_CHARS);
                Candidate {
                    port,
                    pid,
                    world: world::find_active(port, Some(&name)).map(|active| active.world.dir),
                    summary: found.summary(),
                    name,
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default();
    (candidates, ports.games)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(port: u16, pid: u32, world: Option<&str>) -> Candidate {
        Candidate {
            port,
            pid,
            world: world.map(PathBuf::from),
            name: format!("Мир {port}"),
            summary: String::new(),
        }
    }

    fn room(world: Option<&str>, pid: Option<u32>) -> RoomGame {
        RoomGame {
            world: world.map(PathBuf::from),
            pid,
        }
    }

    #[test]
    fn nothing_open_means_wait() {
        assert_eq!(decide(&room(Some("w"), Some(1)), &[], &[1]), Decision::Wait);
    }

    #[test]
    fn same_world_folder_wins_even_in_another_game() {
        // The host restarted the game and opened the room's world; next to it a world of another game.
        let found = [
            candidate(50001, 7, Some("saves/Другой")),
            candidate(50002, 9, Some("saves/Мой мир")),
        ];
        assert_eq!(
            decide(&room(Some("saves/Мой мир"), Some(1)), &found, &[7, 9]),
            Decision::Switch(1)
        );
    }

    #[test]
    fn same_game_is_followed_when_the_world_folder_is_unknown_or_new() {
        let found = [candidate(50001, 7, None), candidate(50002, 1, None)];
        assert_eq!(
            decide(&room(None, Some(1)), &found, &[1, 7]),
            Decision::Switch(1)
        );
        // The host opened a different world in their game: the room follows their game.
        let found = [
            candidate(50001, 7, Some("saves/A")),
            candidate(50002, 1, Some("saves/B")),
        ];
        assert_eq!(
            decide(&room(Some("saves/C"), Some(1)), &found, &[1, 7]),
            Decision::Switch(1)
        );
    }

    #[test]
    fn a_world_of_an_unrelated_game_is_not_taken() {
        // The room's game is still running, and only a world of another game is open.
        let found = [candidate(50001, 7, Some("saves/Чужой"))];
        let decision = decide(&room(Some("saves/Мой мир"), Some(1)), &found, &[1, 7]);
        assert!(
            matches!(&decision, Decision::Unclear(why) if why.contains("50001")),
            "{decision:?}"
        );
    }

    #[test]
    fn the_only_world_is_taken_when_the_room_game_is_gone() {
        // The room's game was closed and started again: it has a new process.
        let found = [candidate(50001, 7, Some("saves/Другой"))];
        assert_eq!(
            decide(&room(Some("saves/Мой мир"), Some(1)), &found, &[7]),
            Decision::Switch(0)
        );
        // Nothing is known about the room (the world on the old port never answered).
        assert_eq!(decide(&room(None, None), &found, &[7]), Decision::Switch(0));
    }

    #[test]
    fn several_candidates_without_a_match_are_ambiguous() {
        let found = [
            candidate(50001, 7, Some("saves/A")),
            candidate(50002, 9, Some("saves/B")),
        ];
        let decision = decide(&room(None, None), &found, &[7, 9]);
        assert!(
            matches!(&decision, Decision::Unclear(why) if why.contains("50001, 50002")),
            "{decision:?}"
        );
        // One game opened two worlds, the room's folder is unknown.
        let found = [candidate(50001, 1, None), candidate(50002, 1, None)];
        assert!(matches!(
            decide(&room(None, Some(1)), &found, &[1]),
            Decision::Unclear(_)
        ));
        // The room's world is on two ports at once (this does not happen, but we will not guess).
        let found = [
            candidate(50001, 1, Some("saves/A")),
            candidate(50002, 7, Some("saves/A")),
        ];
        assert!(matches!(
            decide(&room(Some("saves/A"), Some(1)), &found, &[1, 7]),
            Decision::Unclear(_)
        ));
    }
}
