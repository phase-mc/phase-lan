//! Connection monitoring: path type (direct or via relay), ping,
//! path switches, traffic and losses. The summary is saved to a report (JSON).

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use iroh::{
    TransportAddr,
    endpoint::{Connection, PathEvent},
};
use n0_future::StreamExt;
use serde::Serialize;
use tracing::{debug, info};

use crate::events::{EventSink, PathSample, TunnelEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    Direct,
    Relay,
    Other,
}

impl PathKind {
    pub fn of(addr: &TransportAddr) -> Self {
        if addr.is_ip() {
            Self::Direct
        } else if addr.is_relay() {
            Self::Relay
        } else {
            Self::Other
        }
    }

    /// Path name to show a person.
    pub fn label(self) -> &'static str {
        match self {
            Self::Direct => phase_lang::tr!("прямой", "direct"),
            Self::Relay => "relay",
            Self::Other => phase_lang::tr!("другой", "other"),
        }
    }
}

/// Ping (RTT) summary for one path type.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RttSummary {
    pub samples: u64,
    pub min_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    #[serde(skip)]
    sum_ms: f64,
}

impl RttSummary {
    fn add(&mut self, ms: f64) {
        if self.samples == 0 || ms < self.min_ms {
            self.min_ms = ms;
        }
        if ms > self.max_ms {
            self.max_ms = ms;
        }
        self.samples += 1;
        self.sum_ms += ms;
        self.avg_ms = self.sum_ms / self.samples as f64;
    }
}

/// A change of the path the game data takes.
#[derive(Debug, Clone, Serialize)]
pub struct PathSwitch {
    /// Seconds since the connection started.
    pub at_s: f64,
    pub kind: PathKind,
    pub addr: String,
}

/// Report on one QUIC connection.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionReport {
    pub label: String,
    pub peer: String,
    pub started_unix_s: u64,
    pub duration_s: f64,
    pub selected_path_changes: Vec<PathSwitch>,
    pub rtt_direct: RttSummary,
    pub rtt_relay: RttSummary,
    pub udp_tx_bytes: u64,
    pub udp_rx_bytes: u64,
    pub lost_packets: u64,
    pub lost_bytes: u64,
    pub close_reason: Option<String>,
}

/// Where the core reports connections: reports (saved to a file at the end of the session) and
/// live events for the interface ([`EventSink`]).
#[derive(Debug, Clone, Default)]
pub struct Telemetry {
    reports: Arc<Mutex<Vec<ConnectionReport>>>,
    events: EventSink,
}

impl Telemetry {
    /// Reports only, without live events.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reports and live events.
    pub fn with_events(events: EventSink) -> Self {
        Self {
            reports: Arc::default(),
            events,
        }
    }

    pub fn events(&self) -> &EventSink {
        &self.events
    }

    pub fn emit(&self, event: TunnelEvent) {
        self.events.emit(event);
    }

    /// Reports on closed connections.
    pub fn reports(&self) -> Vec<ConnectionReport> {
        self.reports.lock().expect("reports lock").clone()
    }

    fn push_report(&self, report: ConnectionReport) {
        self.reports.lock().expect("reports lock").push(report);
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Starts monitoring a connection. Every `every` a line with paths and ping is written
/// to the log (plus a [`TunnelEvent::Ping`] event), and on a path change a separate
/// message ([`TunnelEvent::PathSelected`]). After the connection closes, the report goes
/// to `telemetry`.
pub fn spawn_monitor(
    conn: Connection,
    label: String,
    every: Duration,
    telemetry: Telemetry,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let started = Instant::now();
        let mut report = ConnectionReport {
            label: label.clone(),
            peer: conn.remote_id().to_string(),
            started_unix_s: unix_now(),
            duration_s: 0.0,
            selected_path_changes: Vec::new(),
            rtt_direct: RttSummary::default(),
            rtt_relay: RttSummary::default(),
            udp_tx_bytes: 0,
            udp_rx_bytes: 0,
            lost_packets: 0,
            lost_bytes: 0,
            close_reason: None,
        };
        let peer = report.peer.clone();
        record_initial_path(&conn, &label, &mut report, &telemetry);

        let mut events = conn.path_events();
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await; // the first tick fires immediately, skip it

        loop {
            tokio::select! {
                event = events.next() => match event {
                    Some(PathEvent::Selected { remote_addr, .. }) => {
                        let kind = PathKind::of(&remote_addr);
                        info!("[{label}] данные идут по пути: {} ({remote_addr})", kind.label());
                        report.selected_path_changes.push(PathSwitch {
                            at_s: started.elapsed().as_secs_f64(),
                            kind,
                            addr: remote_addr.to_string(),
                        });
                        telemetry.emit(TunnelEvent::PathSelected {
                            peer: peer.clone(),
                            path: kind,
                            addr: remote_addr.to_string(),
                        });
                    }
                    Some(PathEvent::Opened { remote_addr, .. }) => {
                        debug!("[{label}] открыт путь {} ({remote_addr})", PathKind::of(&remote_addr).label());
                    }
                    Some(PathEvent::Closed { remote_addr, .. }) => {
                        debug!("[{label}] закрыт путь {} ({remote_addr})", PathKind::of(&remote_addr).label());
                    }
                    Some(_) => {}
                    None => break,
                },
                _ = tick.tick() => {
                    let paths = sample_paths(&conn, &label, &mut report);
                    telemetry.emit(TunnelEvent::Ping { peer: peer.clone(), paths });
                }
                _ = conn.closed() => break,
            }
        }

        let stats = conn.stats();
        report.duration_s = started.elapsed().as_secs_f64();
        report.udp_tx_bytes = stats.udp_tx.bytes;
        report.udp_rx_bytes = stats.udp_rx.bytes;
        report.lost_packets = stats.lost_packets;
        report.lost_bytes = stats.lost_bytes;
        report.close_reason = conn.close_reason().map(|e| e.to_string());
        telemetry.push_report(report);
    })
}

fn record_initial_path(
    conn: &Connection,
    label: &str,
    report: &mut ConnectionReport,
    telemetry: &Telemetry,
) {
    let paths = conn.paths();
    if let Some(selected) = paths.iter().find(|p| p.is_selected()) {
        let kind = PathKind::of(selected.remote_addr());
        info!(
            "[{label}] данные идут по пути: {} ({})",
            kind.label(),
            selected.remote_addr()
        );
        report.selected_path_changes.push(PathSwitch {
            at_s: 0.0,
            kind,
            addr: selected.remote_addr().to_string(),
        });
        telemetry.emit(TunnelEvent::PathSelected {
            peer: report.peer.clone(),
            path: kind,
            addr: selected.remote_addr().to_string(),
        });
    }
}

/// Measures the ping of all open paths. Paths without traffic also keep the ping current:
/// the check runs every [`crate::endpoint::PATH_KEEP_ALIVE`].
fn sample_paths(conn: &Connection, label: &str, report: &mut ConnectionReport) -> Vec<PathSample> {
    let paths = conn.paths();
    let mut parts = Vec::new();
    let mut samples = Vec::new();
    for path in paths.iter() {
        let kind = PathKind::of(path.remote_addr());
        let rtt_ms = path.rtt().as_secs_f64() * 1000.0;
        match kind {
            PathKind::Direct => report.rtt_direct.add(rtt_ms),
            PathKind::Relay => report.rtt_relay.add(rtt_ms),
            PathKind::Other => {}
        }
        let marker = if path.is_selected() { "▶ " } else { "" };
        parts.push(format!("{marker}{} {rtt_ms:.0} мс", kind.label()));
        samples.push(PathSample {
            path: kind,
            rtt_ms,
            selected: path.is_selected(),
        });
    }
    if parts.is_empty() {
        info!("[{label}] нет открытых путей");
    } else {
        info!("[{label}] пинг: {}", parts.join(" | "));
    }
    samples
}

/// Current ping of the selected path and its type (for the echo report).
pub fn selected_path_rtt(conn: &Connection) -> Option<(PathKind, Duration)> {
    let paths = conn.paths();
    paths
        .iter()
        .find(|p| p.is_selected())
        .map(|p| (PathKind::of(p.remote_addr()), p.rtt()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtt_summary() {
        let mut s = RttSummary::default();
        for v in [30.0, 10.0, 20.0] {
            s.add(v);
        }
        assert_eq!(s.samples, 3);
        assert_eq!(s.min_ms, 10.0);
        assert_eq!(s.max_ms, 30.0);
        assert!((s.avg_ms - 20.0).abs() < 1e-9);
    }
}
