//! The host's room: seats, joining the world, and how guests look in the game.
//!
//! The network core asks [`Room`] what room this is ([`RoomService`]): here
//! the core shows the start of the game stream (which nickname the guest joins the world under) and
//! passes on guests' styling requests. The state is shared by the session tasks; the lock
//! is held only for the duration of a short write.
//!
//! LAN is free for everyone (owner's decision of 2026-11-18): the Phase Pro rights a guest
//! may send in the greeting are not checked by the room and nothing relies on them. The number
//! of seats is set by the host (the "Seats in the room" setting), and whether the game itself lets that many in
//! is decided by Phase Companion in the world ([`crate::look::room_capacity`]).

use std::sync::{Arc, Mutex};

use phase_minecraft::handshake::{self, Sniff};
use phase_tunnel::{
    EndpointId,
    access::RoomGate,
    host::{GameAdmit, GameSniff, RoomService},
    wire::{self, RoomInfo},
};

use crate::{
    look::{Look, VANILLA_MAX_PLAYERS},
    names::NameBindings,
    session::Ctx,
};

#[derive(Debug, Default)]
struct State {
    /// Phase Companion is working (or it is your own server): the integrated server limit is lifted.
    companion: bool,
    /// Nicknames tied to devices (when players are let in without an account check).
    names: NameBindings,
}

/// The host's room. Cheap to clone: all copies are one room.
#[derive(Clone)]
pub(crate) struct Room {
    state: Arc<Mutex<State>>,
    gate: RoomGate,
    look: Look,
    ctx: Ctx,
}

impl std::fmt::Debug for Room {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Room")
            .field("max_players", &self.gate.limit())
            .finish_non_exhaustive()
    }
}

impl Room {
    pub fn new(gate: RoomGate, look: Look, ctx: Ctx) -> Self {
        gate.set_limit(look.max_players());
        Self {
            state: Arc::new(Mutex::new(State::default())),
            gate,
            look,
            ctx,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("room state lock")
    }

    /// How many players the room holds including the host.
    pub fn max_players(&self) -> u32 {
        self.gate.limit()
    }

    /// Seats changed: Phase Companion appeared or disappeared in the world (it lifts the limit
    /// of the integrated server) or the host changed "Seats in the room".
    pub fn set_capacity(&self, companion: bool, max_players: u32) {
        let was_companion = std::mem::replace(&mut self.state().companion, companion);
        let before = self.gate.limit();
        if before != max_players {
            self.gate.set_limit(max_players);
            self.ctx.update(|s| {
                if let Some(host) = &mut s.host {
                    host.room.max_players = max_players;
                }
            });
        }
        let text = match (was_companion != companion, companion) {
            (true, true) => phase_lang::tr!(
                format!("Phase Companion работает в мире: комната до {max_players} игроков."),
                format!(
                    "Phase Companion is running in the world: the room fits up to {max_players} players."
                )
            ),
            (true, false) => phase_lang::tr!(
                format!(
                    "Phase Companion в мире не найден: игра пускает не больше {VANILLA_MAX_PLAYERS}, комната до {max_players} игроков."
                ),
                format!(
                    "Phase Companion was not found in the world: the game allows at most {VANILLA_MAX_PLAYERS}, the room is set to {max_players} players."
                )
            ),
            (false, _) if before != max_players => phase_lang::tr!(
                format!("Комната до {max_players} игроков."),
                format!("The room fits up to {max_players} players.")
            ),
            (false, _) => return,
        };
        self.ctx.info(text);
    }
}

impl RoomService for Room {
    fn hello(&self, _peer: &EndpointId, _entitlement: Option<&str>) {
        // The rights token stays in the protocol for future cosmetics and Phase servers; in LAN
        // it opens nothing and is not checked.
    }

    fn info(&self, _peer: &EndpointId) -> RoomInfo {
        RoomInfo {
            max_players: self.max_players(),
            pro: false,
            look: true,
        }
    }

    fn sniff_game(&self, head: &[u8]) -> GameSniff {
        match handshake::sniff(head) {
            Sniff::NeedMore => GameSniff::NeedMore,
            Sniff::NotLogin => GameSniff::Skip,
            Sniff::Login(login) => GameSniff::Login {
                name: login.name,
                uuid: login.uuid,
            },
        }
    }

    fn admit_game(&self, peer: &EndpointId, name: &str) -> GameAdmit {
        let (account_check, host_name) = self.look.admission();
        if account_check {
            // The account service confirms the nickname: no binding to a device is needed.
            return GameAdmit::Allow;
        }
        let verdict = self
            .state()
            .names
            .admit(&peer.to_string(), name, host_name.as_deref());
        match verdict {
            Ok(()) => GameAdmit::Allow,
            Err(refusal) => GameAdmit::Refuse {
                reason: refusal.log_text(name),
                reply: handshake::login_disconnect(&refusal.game_text(name)),
            },
        }
    }

    fn look(&self, peer: &EndpointId, request: wire::LookRequest) -> wire::LookState {
        self.look.guest_request(&peer.to_string(), request)
    }
}
