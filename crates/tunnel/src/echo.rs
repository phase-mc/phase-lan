//! Echo probe: a channel check without Minecraft.
//!
//! The guest sends short frames at an interval, the host returns them right away.
//! The ping through the tunnel ("echo") is compared with the ping of the path itself (QUIC RTT):
//! the difference shows how much delay our program adds.

use phase_lang::tr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iroh::endpoint::{Connection, ReadExactError, RecvStream, SendStream};
use serde::Serialize;

use crate::{
    metrics::{self, PathKind},
    wire::{self, ECHO_FRAME_LEN, StreamKind},
};

/// How long to wait for the last answers after sending the last frame.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Host side: returns every frame until the guest closes the stream.
pub async fn serve(mut send: SendStream, mut recv: RecvStream) -> Result<()> {
    let mut frame = [0u8; ECHO_FRAME_LEN];
    loop {
        match recv.read_exact(&mut frame).await {
            Ok(()) => send.write_all(&frame).await?,
            Err(ReadExactError::FinishedEarly(_)) => break,
            Err(err) => return Err(err.into()),
        }
    }
    let _ = send.finish();
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct EchoReport {
    pub sent: u32,
    pub received: u32,
    /// Frames without an answer. A QUIC stream is reliable and does not lose frames itself, so answers
    /// go missing only when the connection drops ([`Self::error`]) or on timeout.
    pub unanswered: u32,
    /// The error that interrupted the probe (for example, the connection was lost). Partial data is kept.
    pub error: Option<String>,
    /// The last answers did not arrive in the allotted time.
    pub timed_out: bool,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// The longest pause between two answers: shows "freezes" (test T6).
    pub max_gap_ms: f64,
    /// Path type and its ping (QUIC RTT) at the end of the probe.
    pub path_kind: Option<PathKind>,
    pub path_rtt_ms: Option<f64>,
}

/// Guest side: sends `count` frames every `interval` and measures the ping.
///
/// If the connection drops in the middle of the probe, a report with partial data and
/// the error text is returned: such sessions matter most for diagnostics (test T6).
pub async fn run(conn: &Connection, count: u32, interval: Duration) -> Result<EchoReport> {
    if count == 0 {
        bail!(tr!(
            "количество замеров должно быть больше нуля",
            "the number of probes must be above zero"
        ));
    }
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .context(tr!("не удалось открыть поток", "could not open the stream"))?;
    wire::write_kind(&mut send, StreamKind::Echo).await?;
    let start = Instant::now();

    let sender = async {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sent = 0u32;
        for seq in 0..u64::from(count) {
            tick.tick().await;
            let sent_us = start.elapsed().as_micros() as u64;
            let mut frame = [0u8; ECHO_FRAME_LEN];
            frame[..8].copy_from_slice(&seq.to_be_bytes());
            frame[8..].copy_from_slice(&sent_us.to_be_bytes());
            if let Err(err) = send.write_all(&frame).await {
                return (
                    sent,
                    Some(
                        anyhow::Error::from(err)
                            .context(tr!("отправка прервалась", "sending was interrupted")),
                    ),
                );
            }
            sent += 1;
        }
        let _ = send.finish();
        (sent, None)
    };

    let receiver = async {
        let mut rtts = Vec::with_capacity(count as usize);
        let mut last_reply: Option<Instant> = None;
        let mut max_gap = Duration::ZERO;
        let mut frame = [0u8; ECHO_FRAME_LEN];
        let deadline = interval * count + DRAIN_TIMEOUT;
        let read_all = async {
            while rtts.len() < count as usize {
                match recv.read_exact(&mut frame).await {
                    Ok(()) => {}
                    Err(ReadExactError::FinishedEarly(_)) => break,
                    Err(err) => return Err(anyhow::Error::from(err)),
                }
                let now = Instant::now();
                let sent_us = u64::from_be_bytes(frame[8..].try_into().expect("8 bytes"));
                let rtt = now
                    .duration_since(start)
                    .saturating_sub(Duration::from_micros(sent_us));
                rtts.push(rtt);
                if let Some(prev) = last_reply {
                    max_gap = max_gap.max(now.duration_since(prev));
                }
                last_reply = Some(now);
            }
            Ok(())
        };
        // Do not wait forever if answers stopped coming.
        let (error, timed_out) = match tokio::time::timeout(deadline, read_all).await {
            Ok(Ok(())) => (None, false),
            Ok(Err(err)) => (
                Some(err.context(tr!("приём прервался", "receiving was interrupted"))),
                false,
            ),
            Err(_) => (None, true),
        };
        (rtts, max_gap, error, timed_out)
    };

    let ((sent, send_error), (mut rtts, max_gap, recv_error, timed_out)) =
        tokio::join!(sender, receiver);
    let error = send_error.or(recv_error).map(|err| format!("{err:#}"));
    rtts.sort();
    let received = rtts.len() as u32;
    let pct = |p: f64| -> f64 {
        if rtts.is_empty() {
            return 0.0;
        }
        let idx = ((rtts.len() as f64 - 1.0) * p).round() as usize;
        rtts[idx].as_secs_f64() * 1000.0
    };
    let path = metrics::selected_path_rtt(conn);
    Ok(EchoReport {
        sent,
        received,
        unanswered: sent.saturating_sub(received),
        error,
        timed_out,
        p50_ms: pct(0.50),
        p95_ms: pct(0.95),
        p99_ms: pct(0.99),
        max_ms: rtts.last().map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0),
        max_gap_ms: max_gap.as_secs_f64() * 1000.0,
        path_kind: path.map(|(k, _)| k),
        path_rtt_ms: path.map(|(_, rtt)| rtt.as_secs_f64() * 1000.0),
    })
}
