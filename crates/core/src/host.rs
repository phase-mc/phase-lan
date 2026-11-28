//! Host scenario: find the world → find voice ports → open the node → issue a
//! ticket → accept guests until stopped.

use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use n0_future::task::AbortOnDropHandle;
use phase_lang::tr;
use phase_minecraft::{lan, status, voice as mc_voice};
use phase_tunnel::{
    EndpointTicket,
    access::{AccessHook, AccessList},
    endpoint::{self, EndpointOptions},
    events::{EventSink, TunnelEvent},
    host::{self, GameTarget, HostConfig, RoomOptions},
    identity,
    metrics::Telemetry,
    mods::ModShare,
    wire,
};
use tokio::sync::mpsc;

use crate::{
    events::SessionEvent,
    follow,
    look::{Look, LookSetup},
    modsync,
    options::{
        GamePort, HostOptions, HostVoice, relay_info, resolve_profile_dir, session_nickname,
    },
    profiles::ALLOW_FILE,
    reports,
    room::Room,
    session::{Control, Ctx},
    status::{HostModsStatus, HostReady, Phase, RoomStatus, VoiceMode, VoicePlan, VoicePortInfo},
};

/// How long to wait for the relay connection before issuing the ticket.
pub const RELAY_WAIT: Duration = Duration::from_secs(10);
/// How long to look for an open LAN world with `auto`.
pub const DETECT_WAIT: Duration = Duration::from_secs(10);
/// World name if it could not be learned.
pub const DEFAULT_MOTD: &str = "Мир Phase";

/// The same name in the window's language.
pub fn default_motd() -> &'static str {
    tr!(DEFAULT_MOTD, "Phase World")
}

/// What is known about the game on this PC.
struct Game {
    port: Option<u16>,
    /// Name of the found LAN world.
    motd: Option<String>,
    /// A Minecraft server answers on the port.
    status: Option<status::ServerStatus>,
}

/// Writes to the log what is being distributed (or why distribution does not work).
fn announce_mods(ctx: &Ctx, mods: &HostModsStatus) {
    if let Some(error) = &mods.error {
        ctx.warn(tr!(
            format!("Моды не раздаются: {error}"),
            format!("Mods are not being shared: {error}")
        ));
    } else if mods.sharing && mods.files > 0 {
        let dir = mods.dir.as_deref().unwrap_or("mods");
        ctx.info(tr!(
            format!("Моды: гости получат {} из папки {}.", mods.files, dir),
            format!(
                "Mods: guests will get {} from the folder {}.",
                mods.files, dir
            )
        ));
    } else if mods.sharing {
        ctx.info(tr!(
            "Моды: папка модов пуста — гостям моды не требуются.",
            "Mods: the mods folder is empty, so guests need no mods."
        ));
    }
    if !mods.skipped.is_empty() {
        let skipped = mods.skipped.join(", ");
        ctx.warn(tr!(
            format!("Моды с недопустимыми именами файлов не раздаются: {skipped}."),
            format!("Mods with invalid file names are not shared: {skipped}.")
        ));
    }
}

pub(crate) async fn run(
    opts: HostOptions,
    ctx: Ctx,
    mut control: mpsc::UnboundedReceiver<Control>,
) -> Result<()> {
    ctx.phase(Phase::Preparing);
    let dir = resolve_profile_dir(&opts.profile, &opts.profile_dir)?;
    // Relay settings are checked right away: an error in the certificate file is visible before the world search.
    let relay = opts.net.relay_settings()?;
    let network = opts.net.mode;
    for warning in relay.warnings() {
        ctx.warn(warning);
    }

    let Some(game) = ctx.cancellable(find_game(opts.game_port, &ctx)).await else {
        return Ok(());
    };
    let game = game?;
    let motd = opts
        .motd
        .clone()
        .map(|m| wire::sanitize_text(&m, wire::MOTD_MAX_CHARS))
        .filter(|m| !m.is_empty())
        .or(game.motd.clone())
        .unwrap_or_else(|| default_motd().to_string());
    let game_version = game
        .status
        .as_ref()
        .map(|s| s.version.clone())
        .filter(|v| !v.is_empty());

    let voice = plan_voice(&opts.voice, &game, &ctx).await?;

    // Mods: folder, hashes (with cache), distribution. The first scan of a large pack takes seconds.
    let share = ModShare::new();
    let Some(mods) = ctx
        .cancellable(modsync::apply_host_mods(
            &share,
            &opts.mods,
            &opts.mod_hash_cache,
        ))
        .await
    else {
        return Ok(());
    };
    announce_mods(&ctx, &mods);

    let key = identity::load_or_create_secret_key(&dir)?;
    let device_id = key.public().to_string();
    let nickname = session_nickname(&opts.nickname, &dir, &opts.profile).await?;
    ctx.update(|s| {
        s.device_id = Some(device_id.clone());
        s.nickname = Some(nickname.clone());
    });
    let access = AccessList::load(dir.join(ALLOW_FILE))?;
    for id in &opts.allow {
        if access.allow(*id)? {
            let who = wire::peer_label(id, access.nickname(id).as_deref());
            ctx.info(tr!(
                format!("Устройство {who} добавлено в разрешённые."),
                format!("Device {who} added to the allowed list.")
            ));
        }
    }
    let asks_new_devices = opts.approver.is_some();
    // Styling in the game: header, tags, nickname colors: a datapack in the host's world. The
    // "Seats in the room" setting is there too: everything is free, the host's plan is not asked here.
    let look = Look::new(
        LookSetup {
            profile_dir: dir.clone(),
            host_device: device_id,
            host_nickname: nickname.clone(),
            game_port: game.port,
            motd: game.motd.clone(),
            protocol: game
                .status
                .as_ref()
                .and_then(|status| status.protocol)
                .and_then(|protocol| i32::try_from(protocol).ok()),
            world: opts.world.clone(),
        },
        ctx.clone(),
    );
    let room = Room::new(access.gate().clone(), look.clone(), ctx.clone());
    ctx.info(tr!(
        format!("Комната до {} игроков.", room.max_players()),
        format!("The room holds up to {} players.", room.max_players())
    ));
    let telemetry = Telemetry::with_events(guest_roster(&ctx, &look));
    let hook = AccessHook::new(access.clone(), opts.approver.clone())
        .with_events(telemetry.events().clone());

    ctx.phase(Phase::Connecting);
    let ep = endpoint::bind(EndpointOptions {
        secret_key: key,
        accept_incoming: true,
        network,
        relay: relay.clone(),
        access: Some(hook),
    })
    .await?;

    let Some(addr) = ctx
        .cancellable(endpoint::shareable_addr(&ep, network, RELAY_WAIT))
        .await
    else {
        ep.close().await;
        return Ok(());
    };
    let ticket = EndpointTicket::new(addr);
    // The game port may change while the room runs (the world was opened again, see `follow.rs`).
    let target = GameTarget::new(game.port);
    let cfg = HostConfig {
        game_port: target.clone(),
        motd: motd.clone(),
        nickname: Some(nickname),
        udp_ports: voice.ports.iter().map(|p| p.port).collect(),
        game_version: game_version.clone(),
        // Manually set ports were chosen by the host themselves: we do not check their owner.
        check_voice_owner: voice.mode != VoiceMode::Manual,
        status_every: opts.status_every,
        mods: share.clone(),
        room: RoomOptions {
            max_players: room.max_players(),
            service: Some(Arc::new(room.clone())),
        },
    };
    let ready = HostReady {
        ticket: ticket.to_string(),
        motd,
        game_port: game.port,
        game_version,
        voice,
        relay: relay_info(&relay, network),
        allowed_devices: access.len(),
        asks_new_devices,
        queued: 0,
        guests: Vec::new(),
        mods,
        room: RoomStatus {
            max_players: room.max_players(),
        },
        look: look.status(),
    };
    ctx.update(|s| {
        s.host = Some(ready);
        s.phase = Phase::Running;
    });

    // Styling lives while the room is open: the datapack is rewritten on every
    // change. The tasks below are aborted when this function exits.
    let _look = AbortOnDropHandle::new(tokio::spawn({
        let room = room.clone();
        look.clone()
            .run(move |companion, max_players| room.set_capacity(companion, max_players))
    }));
    // The host left the world and opened it again on a different port: the room follows the world
    // (port found by the program) or suggests the new port (set manually).
    let _follow = opts.game_port.filter(|_| game.port.is_some()).map(|kind| {
        AbortOnDropHandle::new(tokio::spawn(follow::watch(follow::Follow {
            follow: kind.follows_world(),
            target: target.clone(),
            look: look.clone(),
            ctx: ctx.clone(),
        })))
    });
    // Window commands: styling is applied at once, mod distribution by its own task (its
    // rebuild takes seconds and must not hold up the rest).
    let (mods_tx, mut mods_rx) = mpsc::unbounded_channel();
    let _control = AbortOnDropHandle::new(tokio::spawn({
        let (look, access, ctx) = (look.clone(), access.clone(), ctx.clone());
        async move {
            while let Some(command) = control.recv().await {
                match command {
                    Control::HostMods(wanted) => {
                        let _ = mods_tx.send(wanted);
                    }
                    Control::Look(command) => look.command(command),
                    Control::Forget(id, reply) => {
                        let who = wire::peer_label(&id, access.nickname(&id).as_deref());
                        let result = access.revoke(&id);
                        if matches!(result, Ok(true)) {
                            ctx.update(|s| {
                                if let Some(host) = &mut s.host {
                                    host.allowed_devices = access.len();
                                }
                            });
                            ctx.info(tr!(
                                format!("Устройство {who} убрано из разрешённых."),
                                format!("Device {who} removed from the allowed list.")
                            ));
                        }
                        let _ = reply.send(result);
                    }
                    Control::Mods(_) | Control::Glow(_) => {}
                }
            }
        }
    }));
    // The host changes mod distribution without closing the room: we rebuild it in the background.
    let _mods = AbortOnDropHandle::new(tokio::spawn({
        let (ctx, cache_file) = (ctx.clone(), opts.mod_hash_cache.clone());
        async move {
            while let Some(mut wanted) = mods_rx.recv().await {
                // Checkboxes are ticked quickly: we take only the latest state.
                while let Ok(next) = mods_rx.try_recv() {
                    wanted = next;
                }
                let mut fresh = modsync::apply_host_mods(&share, &wanted, &cache_file).await;
                announce_mods(&ctx, &fresh);
                ctx.update(|s| {
                    if let Some(host) = &mut s.host {
                        fresh.sent_files = host.mods.sent_files;
                        fresh.sent_bytes = host.mods.sent_bytes;
                        host.mods = fresh;
                    }
                });
            }
        }
    }));

    let result = ctx
        .cancellable(host::serve(ep.clone(), access, cfg, telemetry.clone()))
        .await
        .unwrap_or(Ok(()));
    ctx.phase(Phase::Stopping);
    ep.close().await;
    // The room is closed: the datapack in the world becomes a cleanup (the bossbar and teams go away).
    look.close().await;
    save_report(&opts, &ctx, &telemetry);
    result
}

/// Events of the host's network core: each goes onto the bus, and connections, nicknames and disconnections
/// of guests also maintain the "who is in the room" list in the session state
/// ([`HostReady::guests`]), so the window knows the guests even after a page reload.
fn guest_roster(ctx: &Ctx, look: &Look) -> EventSink {
    let bus = ctx.bus.tunnel_sink();
    let ctx = ctx.clone();
    let look = look.clone();
    EventSink::new(move |event| {
        let guests = |change: &dyn Fn(&mut HostReady)| {
            ctx.update(|s| {
                if let Some(host) = &mut s.host {
                    change(host);
                }
            })
        };
        match &event {
            TunnelEvent::PeerConnected { peer, nickname } => {
                guests(&|host| host.guest_connected(peer, nickname.as_deref()));
                look.guest_connected(peer, nickname.as_deref());
            }
            TunnelEvent::PeerNickname { peer, nickname } => {
                guests(&|host| host.guest_renamed(peer, nickname));
                look.guest_renamed(peer, nickname);
            }
            TunnelEvent::PeerGameName { peer, name, .. } => look.guest_game_name(peer, name),
            TunnelEvent::GameRefused { name, reason, .. } => {
                ctx.warn(tr!(
                    format!("Вход в мир под ником {name} отклонён: {reason}."),
                    format!("Joining the world as {name} was refused: {reason}.")
                ));
            }
            TunnelEvent::PeerDisconnected { peer, .. } => {
                guests(&|host| host.guest_disconnected(peer));
                look.guest_disconnected(peer);
            }
            TunnelEvent::ModSent { size, .. } => guests(&|host| {
                host.mods.sent_files += 1;
                host.mods.sent_bytes += size;
            }),
            TunnelEvent::Queue { waiting } => guests(&|host| host.queued = *waiting),
            _ => {}
        }
        bus.emit(event);
    })
}

/// The game port: set manually (we check that Minecraft is there) or a found LAN world.
async fn find_game(port_kind: Option<GamePort>, ctx: &Ctx) -> Result<Game> {
    match port_kind {
        None => Ok(Game {
            port: None,
            motd: None,
            status: None,
        }),
        Some(GamePort::Fixed(port) | GamePort::Found(port)) => {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            match status::query(addr, status::DEFAULT_TIMEOUT).await {
                // The world was found by the auto room: we show it as found, with its name.
                Ok(found) if matches!(port_kind, Some(GamePort::Found(_))) => {
                    let name = wire::sanitize_text(&found.motd, wire::MOTD_MAX_CHARS);
                    ctx.bus.send(SessionEvent::WorldFound {
                        name: name.clone(),
                        port,
                        summary: found.summary(),
                    });
                    Ok(Game {
                        port: Some(port),
                        motd: Some(name).filter(|name| !name.is_empty()),
                        status: Some(found),
                    })
                }
                Ok(found) => {
                    ctx.bus.send(SessionEvent::GameChecked {
                        port,
                        summary: Some(found.summary()),
                        error: None,
                    });
                    Ok(Game {
                        port: Some(port),
                        motd: None,
                        status: Some(found),
                    })
                }
                Err(err) => {
                    ctx.bus.send(SessionEvent::GameChecked {
                        port,
                        summary: None,
                        error: Some(format!("{err:#}")),
                    });
                    ctx.warn(tr!(
                        format!(
                            "На порту {port} сейчас не отвечает сервер Minecraft ({err:#}). Если мир \
                             или сервер ещё запускается — это нормально; иначе проверьте номер порта."
                        ),
                        format!(
                            "No Minecraft server is answering on port {port} right now ({err:#}). If the world \
                             or server is still starting, that's fine; otherwise check the port number."
                        )
                    ));
                    Ok(Game {
                        port: Some(port),
                        motd: None,
                        status: None,
                    })
                }
            }
        }
        Some(GamePort::Auto) => {
            ctx.bus.send(SessionEvent::WorldSearch {
                timeout_s: DETECT_WAIT.as_secs(),
            });
            match lan::detect_local_world(DETECT_WAIT).await? {
                Some(world) => {
                    let name = wire::sanitize_text(&world.announcement.motd, wire::MOTD_MAX_CHARS);
                    ctx.bus.send(SessionEvent::WorldFound {
                        name: name.clone(),
                        port: world.announcement.port,
                        summary: world.status.summary(),
                    });
                    Ok(Game {
                        port: Some(world.announcement.port),
                        motd: Some(name),
                        status: Some(world.status),
                    })
                }
                None => bail!(
                    "{}",
                    tr!(
                        "открытый LAN-мир на этом ПК не найден. Откройте мир для сети и запустите \
                         снова, или укажите порт вручную (номер пишется в чате после «Открыть для сети»)",
                        "no world open to LAN was found on this PC. Open your world to LAN and start \
                         again, or enter the port manually (it appears in the chat after \"Open to LAN\")"
                    )
                ),
            }
        }
    }
}

/// Voice ports: set manually, otherwise found automatically (game log +
/// game process ports) plus fallback ones: the game port and 24454.
async fn plan_voice(voice: &HostVoice, game: &Game, ctx: &Ctx) -> Result<VoicePlan> {
    let plan = match (voice, game.port) {
        (HostVoice::Off, _) | (HostVoice::Auto { .. }, None) => VoicePlan {
            mode: VoiceMode::Off,
            ports: Vec::new(),
            hints: Vec::new(),
        },
        (HostVoice::Manual(requested), _) => {
            let ports = manual_ports(requested, ctx);
            VoicePlan {
                mode: VoiceMode::Manual,
                ports: ports
                    .into_iter()
                    .map(|port| VoicePortInfo {
                        port,
                        description: tr!(
                            "задан вручную; программа не проверяет, кто его держит — \
                             убедитесь, что это голосовой сервер",
                            "set manually; Phase doesn't check what is using it — \
                             make sure it is the voice server"
                        )
                        .into(),
                    })
                    .collect(),
                hints: Vec::new(),
            }
        }
        (HostVoice::Auto { game_dirs }, Some(port)) => {
            // We trust the process holding the port only if a Minecraft server answered there.
            let verified = game.status.is_some();
            let game_dirs = game_dirs.clone();
            // Reading logs and port tables are blocking calls.
            let found = tokio::task::spawn_blocking(move || {
                let logs = mc_voice::candidate_logs(&game_dirs);
                mc_voice::detect(port, verified, &logs, &[port, mc_voice::SVC_DEFAULT_PORT])
            })
            .await
            .context(tr!(
                "поиск голосовых портов аварийно завершился",
                "voice port detection crashed"
            ))?;
            for (port, why) in &found.skipped {
                ctx.warn(tr!(
                    format!("Голосовой порт {port} не пересылается: {why}."),
                    format!("Voice port {port} is not forwarded: {why}.")
                ));
            }
            describe_detection(&found)
        }
    };
    Ok(plan)
}

/// What the program found itself and what it took "just in case", so that when there are problems with
/// voice it is visible where the ports came from.
fn describe_detection(found: &mc_voice::VoiceDetection) -> VoicePlan {
    use mc_voice::PortSource;
    let ports = found
        .ports
        .iter()
        .map(|port| VoicePortInfo {
            port: port.port,
            description: match &port.source {
                PortSource::Log { voice_mod, file } => tr!(
                    format!(
                        "{} — найден в журнале игры ({}), порт сейчас открыт игрой",
                        voice_mod.name(),
                        file.display()
                    ),
                    format!(
                        "{} — found in the game log ({}); the game has this port open",
                        voice_mod.name(),
                        file.display()
                    )
                ),
                PortSource::GameProcess => tr!(
                    "стандартный порт, сейчас открыт игрой",
                    "default port, currently open in the game"
                )
                .to_string(),
                PortSource::UnidentifiedGameSocket => tr!(
                    "порт процесса игры (мод по журналу не опознан)",
                    "game process port (the mod isn't recognised from the log)"
                )
                .to_string(),
                PortSource::Fallback => tr!(
                    "запасной стандартный порт (сейчас никем не открыт)",
                    "fallback default port (not open right now)"
                )
                .to_string(),
            },
        })
        .collect();
    let mut hints = Vec::new();
    if let Some(reason) = &found.game_unconfirmed {
        hints.push(tr!(
            format!(
                "Процесс игры не определён ({reason}) — используются только свободные запасные \
                 порты. Если голос не работает, запустите хоста, когда мир уже открыт, или укажите \
                 голосовой порт вручную."
            ),
            format!(
                "The game process wasn't identified ({reason}), so only free fallback ports are \
                 used. If voice chat doesn't work, start hosting once the world is already open, \
                 or enter the voice port manually."
            )
        ));
    } else if found.logs_read.is_empty() {
        hints.push(
            tr!(
                "Журнал игры не найден. Если игра запущена из необычной папки, укажите папку игры.",
                "The game log wasn't found. If the game runs from an unusual folder, set the game folder."
            )
            .to_string(),
        );
    }
    VoicePlan {
        mode: VoiceMode::Auto,
        ports,
        hints,
    }
}

/// Manually set ports: without repeats (the order is kept) and no more than the guest
/// will open.
fn manual_ports(requested: &[u16], ctx: &Ctx) -> Vec<u16> {
    let mut seen = HashSet::new();
    let mut ports: Vec<u16> = requested
        .iter()
        .copied()
        .filter(|p| *p != 0 && seen.insert(*p))
        .collect();
    if ports.len() > mc_voice::MAX_PORTS {
        let dropped = ports.split_off(mc_voice::MAX_PORTS);
        ctx.warn(tr!(
            format!(
                "Голосовых портов больше {}: порты {} не будут пересылаться.",
                mc_voice::MAX_PORTS,
                join_ports(&dropped)
            ),
            format!(
                "More than {} voice ports: ports {} will not be forwarded.",
                mc_voice::MAX_PORTS,
                join_ports(&dropped)
            )
        ));
    }
    ports
}

pub(crate) fn join_ports(ports: &[u16]) -> String {
    ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Saves the session report. A write error does not prevent stopping, only a warning.
fn save_report(opts: &HostOptions, ctx: &Ctx, telemetry: &Telemetry) {
    let Some(dir) = &opts.reports_dir else {
        return;
    };
    match reports::save(dir, "host", telemetry, None) {
        Ok(Some(path)) => ctx.bus.send(SessionEvent::ReportSaved {
            path: path.display().to_string(),
        }),
        Ok(None) => {}
        Err(err) => ctx.warn(tr!(
            format!("Отчёт сессии не сохранён: {err:#}"),
            format!("The session report was not saved: {err:#}")
        )),
    }
}
