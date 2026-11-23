//! Log for the interface: a `tracing` layer that sends every record to a channel.
//!
//! The core writes its log with ordinary `tracing::info!/warn!`. The program window attaches
//! [`LogLayer`] to its `tracing_subscriber` and gets log lines through
//! `broadcast`, without touching the console.

use std::fmt::Write as _;

use serde::Serialize;
use tokio::sync::broadcast;
use tracing::{
    Event, Level, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};

/// How many log lines are kept for a slow subscriber.
pub const LOG_CAPACITY: usize = 2048;

/// A log line.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LogLine {
    /// Record time, Unix milliseconds.
    pub unix_ms: u64,
    /// `error`, `warn`, `info`, `debug`, `trace`.
    pub level: &'static str,
    /// Where the record is from (module), for example `phase_tunnel::guest`.
    pub target: String,
    pub message: String,
}

/// The `tracing` layer → a channel of log lines.
#[derive(Debug, Clone)]
pub struct LogLayer {
    tx: broadcast::Sender<LogLine>,
}

impl LogLayer {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(LOG_CAPACITY);
        Self { tx }
    }

    /// Subscription to new log lines.
    pub fn subscribe(&self) -> broadcast::Receiver<LogLine> {
        self.tx.subscribe()
    }
}

impl Default for LogLayer {
    fn default() -> Self {
        Self::new()
    }
}

fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "error",
        Level::WARN => "warn",
        Level::INFO => "info",
        Level::DEBUG => "debug",
        Level::TRACE => "trace",
    }
}

impl<S: Subscriber> Layer<S> for LogLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        let _ = self.tx.send(LogLine {
            unix_ms: unix_ms(),
            level: level_name(meta.level()),
            target: meta.target().to_string(),
            message: visitor.finish(),
        });
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// Builds the record text: the message + the remaining `name=value` fields.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl MessageVisitor {
    fn finish(mut self) -> String {
        if !self.fields.is_empty() {
            if !self.message.is_empty() {
                self.message.push(' ');
            }
            self.message.push_str(&self.fields);
        }
        self.message
    }

    fn field(&mut self, field: &Field, value: std::fmt::Arguments<'_>) {
        if field.name() == "message" {
            let _ = self.message.write_fmt(value);
        } else {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            let _ = write!(self.fields, "{}={}", field.name(), value);
        }
    }
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.field(field, format_args!("{value}"));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.field(field, format_args!("{value:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn log_lines_reach_subscribers() {
        let layer = LogLayer::new();
        let mut rx = layer.subscribe();
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(port = 25565, "мир не отвечает");
        });
        let line = rx.try_recv().unwrap();
        assert_eq!(line.level, "warn");
        assert_eq!(line.message, "мир не отвечает port=25565");
        assert!(line.target.contains("logs"));
    }
}
