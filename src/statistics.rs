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
mod tests {
    use super::*;
    use crate::pinger::Entry;
    use std::time::Instant;
    use tokio::sync::mpsc;

    fn micros(values: &[u64]) -> Vec<Duration> {
        values.iter().copied().map(Duration::from_micros).collect()
    }

    #[test]
    fn aggregates_are_none_without_samples() {
        assert_eq!(mean(&[]), None);
        assert_eq!(median(&[]), None);
        assert_eq!(std_deviation(&[]), None);
    }

    #[test]
    fn mean_averages_samples() {
        assert_eq!(
            mean(&micros(&[100, 200, 300, 400])),
            Some(Duration::from_micros(250))
        );
        assert_eq!(
            mean(&[Duration::from_secs(10), Duration::from_secs(20)]),
            Some(Duration::from_secs(15))
        );
    }

    #[test]
    fn median_sorts_and_takes_upper_middle_for_even_counts() {
        assert_eq!(
            median(&micros(&[50, 10, 40, 20, 30])),
            Some(Duration::from_micros(30))
        );
        assert_eq!(
            median(&micros(&[40, 10, 30, 20])),
            Some(Duration::from_micros(30))
        );
    }

    #[test]
    fn std_deviation_is_population_deviation() {
        assert_eq!(
            std_deviation(&micros(&[100, 100, 100])),
            Some(Duration::ZERO)
        );
        assert_eq!(
            std_deviation(&micros(&[0, 100])),
            Some(Duration::from_micros(50))
        );
    }

    #[test]
    fn loss_pct_handles_no_partial_and_full_loss() {
        assert_eq!(loss_pct(0, 0), 0.0);
        assert_eq!(loss_pct(4, 2), 50.0);
        assert_eq!(loss_pct(5, 0), 100.0);
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
