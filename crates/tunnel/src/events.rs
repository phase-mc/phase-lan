//! Live network core events, for the interface (the app window, the console).
//!
//! The core prints nothing itself. The text log goes through `tracing`, and everything
//! the interface shows the user (who connected, which path data takes,
//! ping, connection loss, the decision about a new device, the voice summary) is also
//! sent as a typed [`TunnelEvent`] to an [`EventSink`].
//!
//! [`EventSink`] is a plain handler: the core does not know where events go (a
//! `tokio` channel, a Tauri window, the console). The handler is called right in the core's task, so it
//! must be fast and non-blocking (for example, `broadcast::Sender::send`).

use std::{fmt, sync::Arc};

use serde::Serialize;

use crate::{metrics::PathKind, voice::VoiceCounts};

/// Ping of one network path of a connection.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PathSample {
    pub path: PathKind,
    pub rtt_ms: f64,
    /// Data is going over this path now.
    pub selected: bool,
}

/// What the host decided about a new device.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessDecision {
    /// The host said "yes": the device was added to the allowed list.
    Allowed,
    /// The host said "no".
    Denied,
    /// The host already said "no" to this device in this session.
    AlreadyDenied,
    /// Questions are off: unknown devices are not let in.
    PromptsOff,
    /// The host is still answering the question about another device.
    Busy,
    /// The host did not answer in time.
    Timeout,
    /// The device left without waiting for an answer.
    GuestLeft,
    /// No free slots in the room (plan or server limit).
    RoomFull,
}

/// Network core event. `peer` is the full ID of the device on the other side; `nickname` is
/// the game nickname that device gave (already sanitized, see
/// [`crate::wire::sanitize_nickname`]).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TunnelEvent {
    /// Host: a guest passed the check and connected. The nickname comes from the guest's hello or
    /// is the last known one from the allow list; `None` means not known yet.
    PeerConnected {
        peer: String,
        nickname: Option<String>,
    },
    /// Host: a guest gave a nickname (or changed it) after connecting.
    PeerNickname { peer: String, nickname: String },
    /// Host: the guest's game is joining the world under this nickname (read from the start of the game
    /// stream, before encryption). Unlike `nickname`, this is the nickname the game server sees.
    PeerGameName {
        peer: String,
        name: String,
        uuid: Option<String>,
    },
    /// Host: the guest's game was refused entry to the world under this nickname (the host program decided
    /// before the bytes reached the game). `reason` is a readable reason for the log.
    GameRefused {
        peer: String,
        name: String,
        reason: String,
    },
    /// Host: a guest disconnected.
    PeerDisconnected { peer: String, reason: String },
    /// Data switched to another path (direct ↔ relay).
    PathSelected {
        peer: String,
        path: PathKind,
        addr: String,
    },
    /// Regular ping measurement of all open paths.
    Ping {
        peer: String,
        paths: Vec<PathSample>,
    },
    /// Host: a decision about a new device. The nickname is known only if the device managed
    /// to introduce itself (before the "let in?" question).
    Access {
        peer: String,
        nickname: Option<String>,
        decision: AccessDecision,
    },
    /// Host: the game on this PC does not accept the connection (world closed, wrong port).
    GameUnreachable { port: u16, error: String },
    /// Guest: the connection to the host was lost, reconnecting.
    HostLost { reason: String },
    /// Guest: a reconnect attempt failed.
    ReconnectFailed { error: String, retry_in_ms: u64 },
    /// Guest: the connection to the host was restored.
    HostRestored,
    /// Voice summary for the connection (only if voice was used).
    VoiceSummary { peer: String, counts: VoiceCounts },
    /// Host: a guest downloaded a mod directly from this PC. `size` is how many bytes were sent
    /// (less than the file size when resuming).
    ModSent {
        peer: String,
        file_name: String,
        size: u64,
    },
}

type Handler = dyn Fn(TunnelEvent) + Send + Sync;

/// Where the core sends events. Nowhere by default.
#[derive(Clone, Default)]
pub struct EventSink(Option<Arc<Handler>>);

impl EventSink {
    /// The handler is called in the core's task: it must be fast and non-blocking.
    pub fn new(handler: impl Fn(TunnelEvent) + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(handler)))
    }

    pub fn emit(&self, event: TunnelEvent) {
        if let Some(handler) = &self.0 {
            handler(event);
        }
    }
}

impl fmt::Debug for EventSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0.is_some() {
            "EventSink(on)"
        } else {
            "EventSink(off)"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn sink_delivers_and_default_is_silent() {
        EventSink::default().emit(TunnelEvent::HostRestored);

        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = EventSink::new({
            let got = got.clone();
            move |event| got.lock().unwrap().push(event)
        });
        sink.clone().emit(TunnelEvent::HostRestored);
        assert_eq!(*got.lock().unwrap(), vec![TunnelEvent::HostRestored]);
    }

    #[test]
    fn events_serialize_with_kind_tag() {
        let json = serde_json::to_value(TunnelEvent::Access {
            peer: "abc".into(),
            nickname: Some("Steve".into()),
            decision: AccessDecision::Allowed,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "access", "peer": "abc", "nickname": "Steve", "decision": "allowed"
            })
        );
        let json = serde_json::to_value(TunnelEvent::PeerConnected {
            peer: "abc".into(),
            nickname: None,
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "kind": "peer_connected", "peer": "abc", "nickname": null })
        );
    }
}
