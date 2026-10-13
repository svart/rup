use std::time::{Duration, Instant};

use tokio::sync::mpsc::{Receiver, Sender, error::TryRecvError};
use tokio::time::sleep;

use crate::pinger::StatEntry;
use crate::{PingEvent, PingResult};

#[path = "statistics/tracker.rs"]
mod tracker;

use tracker::MeasurementTracker;

pub(crate) fn loss_pct(sent: u64, received: u64) -> f64 {
    if sent > 0 {
        (sent - received) as f64 / sent as f64 * 100.0
    } else {
        0.0
    }
}

pub(crate) fn mean(rtts: &[Duration]) -> Option<Duration> {
    if rtts.is_empty() {
        return None;
    }

    let avg = rtts.iter().sum::<Duration>().as_nanos() / rtts.len() as u128;
    Some(Duration::from_nanos(u64::try_from(avg).unwrap_or(u64::MAX)))
}

pub(crate) fn median(rtts: &[Duration]) -> Option<Duration> {
    let mut sorted = rtts.to_vec();
    sorted.sort();
    sorted.get(sorted.len() / 2).copied()
}

pub(crate) fn std_deviation(rtts: &[Duration]) -> Option<Duration> {
    let avg = mean(rtts)?;
    let variance = rtts
        .iter()
        .map(|value| {
            let diff = avg.as_nanos().abs_diff(value.as_nanos());
            diff * diff
        })
        .sum::<u128>() as f64
        / rtts.len() as f64;
    Some(Duration::from_secs_f64(variance.sqrt() / 1_000_000_000.))
}

enum StatSink {
    None,
    Events(Sender<PingEvent>),
    #[cfg(test)]
    Collector(Sender<PingResult>),
}

impl StatSink {
    async fn event(&self, event: PingEvent) {
        if let StatSink::Events(sender) = self {
            let _ = sender.send(event).await;
        }
    }

    async fn reply(&self, result: PingResult) {
        match self {
            StatSink::None => {}
            StatSink::Events(sender) => {
                let _ = sender.send(PingEvent::Reply(result)).await;
            }
            #[cfg(test)]
            StatSink::Collector(sender) => {
                let _ = sender.send(result).await;
            }
        }
    }
}

async fn signal_generator(to_generator: &Option<Sender<()>>) {
    if let Some(gen_channel) = to_generator {
        let _ = gen_channel.send(()).await;
    }
}

async fn emit_events(events: Vec<PingEvent>, to_generator: &Option<Sender<()>>, sink: &StatSink) {
    for event in events {
        match event {
            PingEvent::Reply(result) => {
                signal_generator(to_generator).await;
                sink.reply(result).await;
            }
            PingEvent::Timeout { seq } => {
                signal_generator(to_generator).await;
                sink.event(PingEvent::Timeout { seq }).await;
            }
            PingEvent::ReorderOrLoss { seq } => {
                sink.event(PingEvent::ReorderOrLoss { seq }).await;
            }
        }
    }
}

async fn drain_ready_stat_entries(
    from_transport: &mut Receiver<StatEntry>,
    tracker: &mut MeasurementTracker,
    to_generator: &Option<Sender<()>>,
    sink: &StatSink,
) -> bool {
    loop {
        match from_transport.try_recv() {
            Ok(entry) => {
                emit_events(tracker.ingest(entry), to_generator, sink).await;
            }
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => return true,
        }
    }
}

async fn recv_send_done(sends_done: &mut Option<Receiver<()>>) {
    if let Some(receiver) = sends_done.as_mut() {
        let _ = receiver.recv().await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn run_statista_core(
    mut from_transport: Receiver<StatEntry>,
    mut sends_done: Option<Receiver<()>>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
    sink: StatSink,
) -> crate::PingReport {
    let mut tracker = MeasurementTracker::new();
    let mut input_closed = false;
    let mut no_more_sends = false;

    loop {
        if (input_closed || no_more_sends) && tracker.is_settled() {
            break;
        }

        let timeout = tracker.next_timeout(Instant::now(), wait_time);

        tokio::select! {
            resp = from_transport.recv(), if !input_closed => {
                let Some(resp) = resp else {
                    input_closed = true;
                    continue;
                };

                emit_events(tracker.ingest(resp), &to_generator, &sink).await;
            }
            _ = recv_send_done(&mut sends_done) => {
                sends_done = None;
                no_more_sends = true;
                input_closed |= drain_ready_stat_entries(
                    &mut from_transport,
                    &mut tracker,
                    &to_generator,
                    &sink,
                ).await;
            }
            _ = sleep(timeout.unwrap_or(wait_time)), if timeout.is_some() => {
                emit_events(
                    tracker.expire(Instant::now(), wait_time),
                    &to_generator,
                    &sink,
                ).await;
            }
        }
    }

    tracker.into_report()
}

pub(crate) async fn statista_report_with_send_done(
    from_transport: Receiver<StatEntry>,
    sends_done: Receiver<()>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
) -> crate::PingReport {
    run_statista_core(
        from_transport,
        Some(sends_done),
        to_generator,
        wait_time,
        StatSink::None,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn statista_with_events(
    from_transport: Receiver<StatEntry>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
    events: Sender<PingEvent>,
) -> crate::PingReport {
    run_statista_core(
        from_transport,
        None,
        to_generator,
        wait_time,
        StatSink::Events(events),
    )
    .await
}

pub(crate) async fn statista_with_events_and_send_done(
    from_transport: Receiver<StatEntry>,
    sends_done: Receiver<()>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
    events: Sender<PingEvent>,
) -> crate::PingReport {
    run_statista_core(
        from_transport,
        Some(sends_done),
        to_generator,
        wait_time,
        StatSink::Events(events),
    )
    .await
}

#[cfg(test)]
async fn statista_with_collector(
    from_transport: Receiver<StatEntry>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
    collector: Sender<PingResult>,
) -> crate::PingReport {
    run_statista_core(
        from_transport,
        None,
        to_generator,
        wait_time,
        StatSink::Collector(collector),
    )
    .await
}

#[cfg(test)]
struct RttSequence {
    rtts: Vec<Duration>,
    sent: u64,
    received: u64,
}

#[cfg(test)]
impl Default for RttSequence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl RttSequence {
    pub fn new() -> Self {
        RttSequence {
            rtts: Vec::with_capacity(1024),
            sent: 0,
            received: 0,
        }
    }

    pub fn record(&mut self, rtt: Duration) {
        self.rtts.push(rtt);
        self.received += 1;
    }

    pub fn set_total_sent(&mut self, n: u64) {
        self.sent = n;
    }

    pub fn mean(&self) -> Duration {
        mean(&self.rtts).unwrap_or_default()
    }

    pub fn std_deviation(&self) -> Duration {
        std_deviation(&self.rtts).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinger::Entry;
    use std::time::Instant;
    use tokio::sync::mpsc;

    #[test]
    fn rtt_sequence_mean() {
        let mut seq = RttSequence::new();
        for i in 0..4 {
            seq.record(Duration::from_micros(100 * (i as u64 + 1)));
        }
        assert_eq!(seq.mean(), Duration::from_micros(250));
    }

    #[test]
    fn rtt_sequence_mean_single_entry() {
        let mut seq = RttSequence::new();
        seq.record(Duration::from_micros(42));
        assert_eq!(seq.mean(), Duration::from_micros(42));
    }

    #[test]
    fn rtt_sequence_mean_large_values() {
        let mut seq = RttSequence::new();
        seq.record(Duration::from_secs(10));
        seq.record(Duration::from_secs(20));
        assert_eq!(seq.mean(), Duration::from_secs(15));
    }

    #[test]
    fn rtt_sequence_median_odd() {
        let mut seq = RttSequence::new();
        for i in 0..5 {
            seq.record(Duration::from_micros((i as u64 + 1) * 10));
        }
        seq.set_total_sent(5);
        seq.rtts.sort();
        assert_eq!(seq.rtts[seq.rtts.len() / 2], Duration::from_micros(30));
    }

    #[test]
    fn rtt_sequence_median_even() {
        let mut seq = RttSequence::new();
        for i in 0..4 {
            seq.record(Duration::from_micros((i as u64 + 1) * 10));
        }
        seq.set_total_sent(4);
        seq.rtts.sort();
        assert_eq!(seq.rtts[seq.rtts.len() / 2], Duration::from_micros(30));
    }

    #[test]
    fn rtt_sequence_single_entry() {
        let mut seq = RttSequence::new();
        seq.record(Duration::from_micros(100));
        seq.set_total_sent(1);
        seq.rtts.sort();
        assert_eq!(seq.rtts.len(), 1);
        assert_eq!(seq.rtts[0], Duration::from_micros(100));
        assert_eq!(seq.sent, 1);
        assert_eq!(seq.received, 1);
    }

    #[test]
    fn rtt_sequence_large_dataset() {
        let mut seq = RttSequence::new();
        for i in 0..1000 {
            seq.record(Duration::from_nanos(i));
        }
        seq.set_total_sent(1000);
        assert_eq!(seq.received, 1000);
        assert!(!seq.rtts.is_empty());
    }

    #[test]
    fn rtt_sequence_std_dev_zero() {
        let mut seq = RttSequence::new();
        for _ in 0..3 {
            seq.record(Duration::from_micros(100));
        }
        assert_eq!(seq.std_deviation(), Duration::from_nanos(0));
    }

    #[test]
    fn rtt_sequence_std_dev_known() {
        let mut seq = RttSequence::new();
        seq.record(Duration::from_micros(0));
        seq.record(Duration::from_micros(100));
        let std_dev = seq.std_deviation();
        assert!(std_dev.as_micros() > 0);
    }

    #[test]
    fn rtt_sequence_exact_sent_tracking() {
        let mut seq = RttSequence::new();
        seq.record(Duration::from_micros(50));
        seq.set_total_sent(10);
        assert_eq!(seq.sent, 10);
        assert_eq!(seq.received, 1);
    }

    #[test]
    fn rtt_sequence_loss_no_sent() {
        let mut seq = RttSequence::new();
        seq.set_total_sent(0);
        let loss_pct = if seq.sent > 0 {
            (seq.sent - seq.received) as f64 / seq.sent as f64 * 100.0
        } else {
            0.0
        };
        assert_eq!(loss_pct, 0.0);
    }

    #[test]
    fn rtt_sequence_loss_partial() {
        let mut seq = RttSequence::new();
        seq.record(Duration::from_micros(1));
        seq.record(Duration::from_micros(2));
        seq.set_total_sent(4);
        let loss_pct = (seq.sent - seq.received) as f64 / seq.sent as f64 * 100.0;
        assert!((loss_pct - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rtt_sequence_full_loss() {
        let mut seq = RttSequence::new();
        seq.set_total_sent(5);
        let loss_pct = (seq.sent - seq.received) as f64 / seq.sent as f64 * 100.0;
        assert!((loss_pct - 100.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn statista_collector_matches_open_close() {
        let (stat_tx, stat_rx) = mpsc::channel(8);
        let (result_tx, mut result_rx) = mpsc::channel(8);
        let sent = Instant::now();
        let received = sent + Duration::from_millis(5);

        stat_tx
            .send(StatEntry::Open(Entry { id: 7, ts: sent }))
            .await
            .unwrap();
        stat_tx
            .send(StatEntry::Close(crate::Response {
                id: 7,
                timestamp: received,
                size: 128,
                ttl: Some(64),
            }))
            .await
            .unwrap();
        drop(stat_tx);

        let report =
            statista_with_collector(stat_rx, None, Duration::from_secs(1), result_tx).await;
        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 1);
        assert_eq!(report.rtts, vec![Duration::from_millis(5)]);

        let result = result_rx.recv().await.unwrap();
        assert_eq!(result.seq, 7);
        assert_eq!(result.rtt, Duration::from_millis(5));
        assert_eq!(result.size, 128);
        assert_eq!(result.ttl, Some(64));
        assert!(result_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn statista_collector_reports_timeout_loss() {
        let (stat_tx, stat_rx) = mpsc::channel(8);
        let (result_tx, mut result_rx) = mpsc::channel(8);

        stat_tx
            .send(StatEntry::Open(Entry {
                id: 0,
                ts: Instant::now(),
            }))
            .await
            .unwrap();

        let handle = tokio::spawn(statista_with_collector(
            stat_rx,
            None,
            Duration::from_millis(1),
            result_tx,
        ));

        tokio::time::sleep(Duration::from_millis(5)).await;
        drop(stat_tx);
        let report = handle.await.unwrap();

        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 0);
        assert!(report.rtts.is_empty());
        assert!(result_rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn statista_timeout_signals_adaptive_generator() {
        let (stat_tx, stat_rx) = mpsc::channel(8);
        let (result_tx, _result_rx) = mpsc::channel(8);
        let (gen_tx, mut gen_rx) = mpsc::channel(8);

        stat_tx
            .send(StatEntry::Open(Entry {
                id: 0,
                ts: Instant::now(),
            }))
            .await
            .unwrap();

        let handle = tokio::spawn(statista_with_collector(
            stat_rx,
            Some(gen_tx),
            Duration::from_millis(1),
            result_tx,
        ));

        tokio::time::timeout(Duration::from_millis(100), gen_rx.recv())
            .await
            .unwrap()
            .unwrap();

        drop(stat_tx);
        let _ = handle.await.unwrap();
    }

    #[tokio::test]
    async fn statista_skips_lost_entry_on_later_close() {
        let (stat_tx, stat_rx) = mpsc::channel(8);
        let (result_tx, mut result_rx) = mpsc::channel(8);
        let sent = Instant::now();

        stat_tx
            .send(StatEntry::Open(Entry { id: 0, ts: sent }))
            .await
            .unwrap();
        stat_tx
            .send(StatEntry::Open(Entry { id: 1, ts: sent }))
            .await
            .unwrap();
        stat_tx
            .send(StatEntry::Close(crate::Response {
                id: 1,
                timestamp: sent + Duration::from_millis(3),
                size: 0,
                ttl: None,
            }))
            .await
            .unwrap();
        drop(stat_tx);

        let report =
            statista_with_collector(stat_rx, None, Duration::from_secs(1), result_tx).await;
        assert_eq!(report.sent, 2);
        assert_eq!(report.received, 1);
        assert_eq!(result_rx.recv().await.unwrap().seq, 1);
    }

    #[tokio::test]
    async fn statista_matches_close_before_open() {
        let (stat_tx, stat_rx) = mpsc::channel(8);
        let (result_tx, mut result_rx) = mpsc::channel(8);
        let sent = Instant::now();

        stat_tx
            .send(StatEntry::Close(crate::Response {
                id: 0,
                timestamp: sent + Duration::from_millis(4),
                size: 0,
                ttl: None,
            }))
            .await
            .unwrap();
        stat_tx
            .send(StatEntry::Open(Entry { id: 0, ts: sent }))
            .await
            .unwrap();
        drop(stat_tx);

        let report =
            statista_with_collector(stat_rx, None, Duration::from_secs(1), result_tx).await;
        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 1);
        assert_eq!(report.rtts, vec![Duration::from_millis(4)]);
        assert_eq!(result_rx.recv().await.unwrap().seq, 0);
    }

    #[tokio::test]
    async fn statista_events_report_reply_timeout_and_reorder() {
        let (stat_tx, stat_rx) = mpsc::channel(8);
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let sent = Instant::now();

        stat_tx
            .send(StatEntry::Open(Entry { id: 0, ts: sent }))
            .await
            .unwrap();
        stat_tx
            .send(StatEntry::Open(Entry { id: 1, ts: sent }))
            .await
            .unwrap();
        stat_tx
            .send(StatEntry::Close(crate::Response {
                id: 1,
                timestamp: sent + Duration::from_millis(3),
                size: 64,
                ttl: Some(63),
            }))
            .await
            .unwrap();
        stat_tx
            .send(StatEntry::Open(Entry {
                id: 2,
                ts: Instant::now(),
            }))
            .await
            .unwrap();

        let handle = tokio::spawn(statista_with_events(
            stat_rx,
            None,
            Duration::from_millis(1),
            event_tx,
        ));

        tokio::time::sleep(Duration::from_millis(5)).await;
        drop(stat_tx);
        let report = handle.await.unwrap();

        assert_eq!(report.sent, 3);
        assert_eq!(report.received, 1);
        assert_eq!(
            event_rx.recv().await.unwrap(),
            PingEvent::ReorderOrLoss { seq: 0 }
        );
        assert_eq!(
            event_rx.recv().await.unwrap(),
            PingEvent::Reply(PingResult {
                seq: 1,
                rtt: Duration::from_millis(3),
                received_at: sent + Duration::from_millis(3),
                size: 64,
                ttl: Some(63),
            })
        );
        assert_eq!(
            event_rx.recv().await.unwrap(),
            PingEvent::Timeout { seq: 2 }
        );
        assert!(event_rx.recv().await.is_none());
    }
}
