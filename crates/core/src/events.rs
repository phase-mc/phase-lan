//! Session events: everything the interface shows the user as work goes on.
//!
//! The current session state is in [`crate::SessionStatus`] (a `watch` channel: you can always
//! learn what is happening now). Here is the event stream (a `broadcast` channel): what
//! happened. There can be any number of subscribers (window, console, log); a slow
//! subscriber skips old events but does not hold the core up.

use phase_tunnel::events::{EventSink, TunnelEvent};
use serde::Serialize;
use tokio::sync::broadcast;

/// How many events are kept for a slow subscriber.
pub const EVENT_CAPACITY: usize = 1024;

/// Importance of a message for the user.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// A session event. In JSON: `{ "type": "...", "data": { ... } }`.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SessionEvent {
    /// A message for the user (what the console version prints as a line).
    Notice { level: Level, text: String },
    /// Host: looking for an open LAN world on this PC.
    WorldSearch { timeout_s: u64 },
    /// Host: an open LAN world was found.
    WorldFound {
        name: String,
        port: u16,
        summary: String,
    },
    /// Host: the room's world is open to the network again, but on a different port (the host left the world and
    /// opened it again). `followed` means the room moved to port `to` by itself (same code,
    /// guests will join as before); `false` means port `from` was set manually, the room stayed
    /// on it, and this is a hint to the host.
    WorldMoved {
        from: u16,
        to: u16,
        name: String,
        summary: String,
        followed: bool,
    },
    /// Host: check of a manually specified port (`summary` is the Minecraft server's reply).
    GameChecked {
        port: u16,
        summary: Option<String>,
        error: Option<String>,
    },
    /// Host: a new device is waiting for an answer to "let in?" ([`crate::ApprovalQueue::answer`]).
    /// `nickname` is the in-game nickname the device gave (`None` means it did not introduce itself).
    ApprovalRequested {
        request_id: u64,
        device_id: String,
        nickname: Option<String>,
    },
    /// The "let in?" question is closed: an answer was received, time ran out or the device left.
    ApprovalClosed { request_id: u64 },
    /// An event of the network core: connections, paths, ping, drops, voice.
    Tunnel(TunnelEvent),
    /// The session report was saved to a file.
    ReportSaved { path: String },
}

/// Event bus: one sender for all of the program's sessions, any number of subscribers.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<SessionEvent>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(EVENT_CAPACITY);
        Self { tx }
    }

    /// Subscription to events sent **after** this call.
    pub fn subscribe(&self) -> broadcast::Receiver<SessionEvent> {
        self.tx.subscribe()
    }

    /// Send an event. If there are no subscribers, the event is simply lost.
    pub fn send(&self, event: SessionEvent) {
        let _ = self.tx.send(event);
    }

    pub fn notice(&self, level: Level, text: impl Into<String>) {
        self.send(SessionEvent::Notice {
            level,
            text: text.into(),
        });
    }

    /// A handler for the network core: its events arrive as [`SessionEvent::Tunnel`].
    pub fn tunnel_sink(&self) -> EventSink {
        let tx = self.tx.clone();
        EventSink::new(move |event| {
            let _ = tx.send(SessionEvent::Tunnel(event));
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape_is_type_and_data() {
        let json = serde_json::to_value(SessionEvent::Tunnel(TunnelEvent::HostRestored)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "tunnel", "data": { "kind": "host_restored" } })
        );
        let json = serde_json::to_value(SessionEvent::Notice {
            level: Level::Warn,
            text: "x".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "notice", "data": { "level": "warn", "text": "x" } })
        );
    }

    #[tokio::test]
    async fn tunnel_events_reach_subscribers() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        bus.tunnel_sink().emit(TunnelEvent::HostRestored);
        assert_eq!(
            rx.recv().await.unwrap(),
            SessionEvent::Tunnel(TunnelEvent::HostRestored)
        );
    }
}
