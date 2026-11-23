//! Phase: host and guest sessions for any interface (console, program window).
//!
//! The core prints nothing. Everything the user sees is passed through channels:
//!
//! | What | Channel | Where |
//! |---|---|---|
//! | Session state right now (stage, ticket, address for the game, error) | `watch` | [`Session::watch_status`] |
//! | What happened (world found, guest connected, ping, drop, voice…) | `broadcast` | [`EventBus::subscribe`] |
//! | The "let the new device in?" question and its answer | event + call | [`ApprovalQueue`] |
//! | Text log (`tracing`) | `broadcast` | [`logs::LogLayer`] |
//!
//! Example (host):
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use phase_core::{EventBus, HostOptions, GamePort, Session};
//!
//! let bus = EventBus::new();
//! let mut events = bus.subscribe(); // subscribe before starting so nothing is missed
//! let mut opts = HostOptions::new("default");
//! opts.game_port = Some(GamePort::Auto);
//! let session = Session::host(opts, bus.clone());
//! let mut status = session.watch_status();
//! status.wait_for(|s| s.host.is_some() || s.phase.is_finished()).await?;
//! println!("Ticket: {:?}", session.status().host.map(|h| h.ticket));
//! session.shutdown().await
//! # }
//! ```

mod approval;
pub mod companion;
pub mod entitlement;
pub mod events;
mod follow;
mod guest;
mod host;
pub mod logs;
pub mod look;
mod modrinth;
pub mod modsync;
mod names;
mod options;
pub mod profiles;
pub mod reports;
mod room;
mod session;
pub mod status;

pub use approval::{ApprovalQueue, PendingApproval};
pub use events::{EventBus, Level, SessionEvent};
pub use guest::with_relay_hint;
pub use host::{DEFAULT_MOTD, DETECT_WAIT, RELAY_WAIT};
pub use look::{
    LookCommand, LookMode, LookPlayer, LookSettings, LookStatus, PlayerStyle, WorldSource,
};
pub use options::{
    DEFAULT_PROFILE, DEFAULT_STATUS_EVERY, GamePort, GuestMods, HostMods, HostOptions, HostVoice,
    JoinOptions, LEGACY_RELAY_TOKEN_ENV, ModrinthSource, ModsFolder, NetOptions, RELAY_TOKEN_ENV,
    relay_token_from_env,
};
pub use session::{ModCommand, Session};
pub use status::{
    GuestLook, GuestPeer, GuestReady, HostModsStatus, HostReady, ModProgress, ModSource,
    ModSyncState, ModSyncStatus, Phase, RelayInfo, Role, RoomStatus, SessionStatus, VoiceMode,
    VoicePlan, VoicePortInfo,
};

/// Program version.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Version of the protocol between host and guest.
pub const PROTOCOL_VERSION: u32 = phase_tunnel::wire::PROTOCOL_VERSION;
