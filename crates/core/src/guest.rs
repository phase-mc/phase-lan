//! Guest scenario: connect by ticket → open a local address for Minecraft →
//! announce the world in "Local network" → keep working (with reconnects) until stopped.

use std::time::Duration;

use anyhow::Result;
use n0_future::task::AbortOnDropHandle;
use phase_lang::tr;
use phase_minecraft::lan::{self, AnnounceMode, AnnounceTarget, Announcement};
use phase_tunnel::{
    Connection,
    endpoint::{self, EndpointOptions, NetworkMode, RelaySettings},
    events::TunnelEvent,
    guest::{self, GuestConfig},
    identity,
    metrics::Telemetry,
    voice::{self, VoicePorts},
    wire,
};
use tokio::sync::{mpsc, watch};

use crate::{
    events::SessionEvent,
    modsync::GuestSync,
    options::{JoinOptions, relay_info, resolve_profile_dir, session_nickname},
    reports,
    session::{Control, Ctx},
    status::{GuestReady, Phase, RoomStatus},
};

pub(crate) async fn run(
    opts: JoinOptions,
    ctx: Ctx,
    control: mpsc::UnboundedReceiver<Control>,
) -> Result<()> {
    ctx.phase(Phase::Preparing);
    let dir = resolve_profile_dir(&opts.profile, &opts.profile_dir)?;
    let host_addr = opts.ticket.endpoint_addr().clone();
    // The guest uses the host's own relay (from the ticket) itself: that is how traffic goes through it.
    let relay = opts.net.relay_settings()?.with_relays_of(&host_addr);
    let network = opts.net.mode;
    for warning in relay.warnings() {
        ctx.warn(warning);
    }
    let key = identity::load_or_create_secret_key(&dir)?;
    let device_id = key.public().to_string();
    let nickname = session_nickname(&opts.nickname, &dir, &opts.profile).await?;
    ctx.info(tr!(
        format!("Ваш ник: {nickname}. ID этого устройства: {device_id}"),
        format!("Your nickname: {nickname}. This device's ID: {device_id}")
    ));
    ctx.update(|s| {
        s.device_id = Some(device_id);
        s.nickname = Some(nickname.clone());
    });

    ctx.phase(Phase::Connecting);
    let ep = endpoint::bind(EndpointOptions {
        secret_key: key,
        accept_incoming: false,
        network,
        relay: relay.clone(),
        access: None,
    })
    .await?;
    ctx.info(tr!(
        format!(
            "Подключаюсь к хосту {}… Если хост вас ещё не одобрял, он увидит вопрос «пустить?» с вашим ником и ID этого устройства.",
            wire::short_id(&host_addr.id)
        ),
        format!(
            "Connecting to host {}… If the host hasn't approved you yet, they'll be asked \"Let them in?\" with your nickname and this device's ID.",
            wire::short_id(&host_addr.id)
        )
    ));

    let telemetry = Telemetry::with_events(ctx.bus.tunnel_sink());
    // LAN is free: the host does not need Phase Pro rights, the token is not sent in the greeting.
    let cfg = GuestConfig {
        host: host_addr.clone(),
        nickname: Some(nickname),
        entitlement: None,
        local_port: opts.local_port,
        voice: if opts.voice {
            VoicePorts::SameAsHost
        } else {
            VoicePorts::Off
        },
        status_every: opts.status_every,
    };
    let started = ctx
        .cancellable(guest::start(ep.clone(), cfg, telemetry.clone()))
        .await;
    let handle = match started {
        None => {
            ep.close().await;
            return Ok(());
        }
        Some(Err(err)) => {
            let err = with_relay_hint(err, &ep, &relay, network);
            ep.close().await;
            return Err(err);
        }
        Some(Ok(handle)) => handle,
    };

    let motd = handle.host_info.motd.clone();
    let local = handle.local_addr;
    for (port, reason) in &handle.voice_skipped {
        ctx.warn(tr!(
            format!("Голосовой порт {port} не открыт: {reason}."),
            format!("Voice port {port} is not open: {reason}.")
        ));
    }
    let mod_sync = GuestSync {
        ctx: ctx.clone(),
        opts: opts.mods.clone(),
        cache_file: opts.mod_hash_cache.clone(),
        conn: handle.watch_connection(),
        offer: handle.host_info.mods,
    };
    let ready = GuestReady {
        host_id: host_addr.id.to_string(),
        host_nickname: handle.host_info.nickname.clone(),
        motd: motd.clone(),
        local_addr: local.to_string(),
        lan_name: (opts.announce != AnnounceMode::Off).then(|| format!("{motd}{}", lan::MOTD_TAG)),
        game_version: handle.host_info.game_version.clone(),
        voice_ports: handle.voice_bound.iter().map(|b| b.host_port).collect(),
        relay: relay_info(&relay, network),
        mods: mod_sync.initial_status(),
        room: handle.host_info.room.map(|room| RoomStatus {
            max_players: room.max_players,
        }),
        look: None,
    };
    ctx.update(|s| {
        s.guest = Some(ready);
        s.phase = Phase::Running;
    });

    // Window commands: mods go to the sync task, highlight to the styling task.
    let (mods_tx, mods_rx) = mpsc::unbounded_channel();
    let (glow_tx, glow_rx) = mpsc::unbounded_channel();
    let _control = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut control = control;
        while let Some(command) = control.recv().await {
            match command {
                Control::Mods(_) => {
                    let _ = mods_tx.send(command);
                }
                Control::Glow(glow) => {
                    let _ = glow_tx.send(glow);
                }
                Control::HostMods(_) | Control::Look(_) | Control::Forget(..) => {}
            }
        }
    }));
    // Mod checking and syncing run alongside the game: joining the world does not depend on them.
    let _mods = AbortOnDropHandle::new(tokio::spawn(mod_sync.run(mods_rx)));
    // How this device looks in the host's world. A host of an old version does not know such a stream,
    // so we do not ask it.
    let _look = handle
        .host_info
        .room
        .is_some_and(|room| room.look)
        .then(|| {
            AbortOnDropHandle::new(tokio::spawn(watch_look(
                ctx.clone(),
                handle.watch_connection(),
                glow_rx,
            )))
        });

    // Announcing the world in "Local network" and watching the connection live as long as
    // the session does: the tasks are aborted when this function exits.
    let _announcer = AbortOnDropHandle::new(tokio::spawn({
        let announcer = lan::run_announcer(
            opts.announce,
            AnnounceTarget::default(),
            Announcement {
                motd,
                port: local.port(),
            },
            handle.online.clone(),
        );
        let ctx = ctx.clone();
        async move {
            if let Err(err) = announcer.await {
                ctx.warn(tr!(
                    format!(
                        "Мир не будет показан в «Локальной сети»: {err:#}. Используйте «Прямое подключение» → {local}"
                    ),
                    format!(
                        "The world won't show up under LAN World: {err:#}. Use Direct Connection → {local}"
                    )
                ));
            }
        }
    }));
    let _online = AbortOnDropHandle::new(tokio::spawn({
        let mut online = handle.online.clone();
        let ctx = ctx.clone();
        async move {
            while online.changed().await.is_ok() {
                let phase = if *online.borrow_and_update() {
                    Phase::Running
                } else {
                    Phase::Reconnecting
                };
                ctx.update(|s| {
                    // We do not overwrite a stop.
                    if matches!(s.phase, Phase::Running | Phase::Reconnecting) {
                        s.phase = phase;
                    }
                });
            }
        }
    }));

    let voice_stats = handle.voice_stats();
    // If we stop waiting on `wait`, the guest stops (see GuestHandle).
    let result = ctx.cancellable(handle.wait()).await.unwrap_or(Ok(()));
    ctx.phase(Phase::Stopping);
    ep.close().await;

    voice::log_summary("гость", &voice_stats);
    let counts = voice_stats.snapshot();
    if counts.is_used() {
        ctx.bus
            .send(SessionEvent::Tunnel(TunnelEvent::VoiceSummary {
                peer: host_addr.id.to_string(),
                counts,
            }));
    }
    if let Some(dir) = &opts.reports_dir {
        let extra = serde_json::json!({ "voice": counts });
        match reports::save(dir, "join", &telemetry, Some(extra)) {
            Ok(Some(path)) => ctx.bus.send(SessionEvent::ReportSaved {
                path: path.display().to_string(),
            }),
            Ok(None) => {}
            Err(err) => ctx.warn(tr!(
                format!("Отчёт сессии не сохранён: {err:#}"),
                format!("Session report not saved: {err:#}")
            )),
        }
    }
    result
}

/// How often the guest asks the host about its styling: the host may have changed it.
const LOOK_EVERY: Duration = Duration::from_secs(3);

/// Watches how this device looks in the host's world ([`GuestReady::look`]) and passes
/// the guest's requests to turn highlight on or off for themselves to the host. The host decides: the window
/// shows what it answered.
async fn watch_look(
    ctx: Ctx,
    conn: watch::Receiver<Option<Connection>>,
    mut glow: mpsc::UnboundedReceiver<bool>,
) {
    let mut shown: Option<wire::LookState> = None;
    let mut wanted: Option<bool> = None;
    loop {
        let current = conn.borrow().clone();
        if let Some(current) = current {
            match guest::look(
                &current,
                wire::LookRequest {
                    glow: wanted.take(),
                },
            )
            .await
            {
                Ok(state) => {
                    if shown.as_ref() != Some(&state) {
                        shown = Some(state.clone());
                        ctx.update(|s| {
                            if let Some(guest) = &mut s.guest {
                                guest.look = Some(state.into());
                            }
                        });
                    }
                }
                Err(err) => tracing::debug!("оформление у хоста не получено: {err:#}"),
            }
        }
        tokio::select! {
            command = glow.recv() => match command {
                Some(glow) => wanted = Some(glow),
                None => return,
            },
            _ = tokio::time::sleep(LOOK_EVERY) => {}
        }
    }
}

/// Adds a hint about your own relay to a connection error if there is no connection to it
/// and the reason is not given yet (`guest::connect_once` adds it when the relay refused).
pub fn with_relay_hint(
    err: anyhow::Error,
    ep: &phase_tunnel::Endpoint,
    relay: &RelaySettings,
    network: NetworkMode,
) -> anyhow::Error {
    if endpoint::relay_problem(ep).is_some() {
        return err;
    }
    match endpoint::relay_hint(ep, relay, network) {
        Some(hint) => err.context(hint),
        None => err,
    }
}
