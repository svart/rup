#[cfg(test)]
use std::sync::Arc;
use std::time::Duration;
use std::{cmp::Ordering, collections::VecDeque};

#[cfg(test)]
use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;

use crate::pinger::{Entry, StatEntry};
use crate::{PingEvent, PingResult};

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

#[cfg(test)]
async fn receive_timeout(
    index: u64,
    req_mutex: Arc<Mutex<VecDeque<Entry>>>,
    wait_time: Duration,
    to_generator: Option<Sender<()>>,
) {
    sleep(wait_time).await;

    let mut requests = req_mutex.lock().await;

    while let Some(req) = requests.front() {
        if req.id <= index {
            requests.pop_front();

            if let Some(gen_channel) = &to_generator {
                let _ = gen_channel.send(()).await;
            }
        } else {
            break;
        }
    }
}

enum StatSink {
    None,
    Events(Sender<PingEvent>),
    Collector(Sender<PingResult>),
}

async fn signal_generator(to_generator: &Option<Sender<()>>) {
    if let Some(gen_channel) = to_generator {
        let _ = gen_channel.send(()).await;
    }
}

async fn emit_reply(sink: &StatSink, seq: u64, rtt: Duration, size: usize, ttl: Option<u8>) {
    let result = PingResult {
        seq,
        rtt,
        size,
        ttl,
    };

    match sink {
        StatSink::None => {}
        StatSink::Events(sender) => {
            let _ = sender.send(PingEvent::Reply(result)).await;
        }
        StatSink::Collector(sender) => {
            let _ = sender.send(result).await;
        }
    }
}

async fn emit_event(sink: &StatSink, event: PingEvent) {
    if let StatSink::Events(sender) = sink {
        let _ = sender.send(event).await;
    }
}

async fn expire_timed_out(
    requests: &mut VecDeque<Entry>,
    wait_time: Duration,
    to_generator: &Option<Sender<()>>,
    sink: &StatSink,
) {
    while let Some(req) = requests.front() {
        if req.ts.elapsed() < wait_time {
            break;
        }

        let req = requests.pop_front().expect("front checked above");
        emit_event(sink, PingEvent::Timeout { seq: req.id }).await;
        signal_generator(to_generator).await;
    }
}

async fn run_statista_core(
    mut from_transport: Receiver<StatEntry>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
    sink: StatSink,
) -> crate::PingReport {
    let mut requests = VecDeque::<Entry>::new();
    let mut rtts = Vec::new();
    let mut total_sent = 0u64;

    loop {
        let timeout = requests
            .front()
            .map(|req| wait_time.saturating_sub(req.ts.elapsed()))
            .unwrap_or(wait_time);

        tokio::select! {
            resp = from_transport.recv() => {
                let Some(resp) = resp else { break; };

                match resp {
                    StatEntry::Open(t) => {
                        total_sent += 1;
                        requests.push_back(t);
                    }
                    StatEntry::Close(t) => {
                        let index = t.id;

                        while let Some(req) = requests.pop_front() {
                            match index.cmp(&req.id) {
                                Ordering::Greater => {
                                    emit_event(&sink, PingEvent::ReorderOrLoss { seq: req.id })
                                        .await;
                                    continue;
                                }
                                Ordering::Equal => {
                                    let rtt = t.timestamp.duration_since(req.ts);
                                    rtts.push(rtt);
                                    signal_generator(&to_generator).await;
                                    emit_reply(&sink, index, rtt, t.size, t.ttl).await;
                                }
                                Ordering::Less => requests.push_front(req),
                            }
                            break;
                        }
                    }
                }
            }
            _ = sleep(timeout), if !requests.is_empty() => {
                expire_timed_out(&mut requests, wait_time, &to_generator, &sink).await;
            }
        }
    }

    crate::PingReport {
        sent: total_sent,
        received: rtts.len() as u64,
        rtts,
    }
}

pub async fn statista(
    from_transport: Receiver<StatEntry>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
) {
    let _ = statista_report(from_transport, to_generator, wait_time).await;
}

pub(crate) async fn statista_report(
    from_transport: Receiver<StatEntry>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
) -> crate::PingReport {
    run_statista_core(from_transport, to_generator, wait_time, StatSink::None).await
}

pub(crate) async fn statista_with_events(
    from_transport: Receiver<StatEntry>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
    events: Sender<PingEvent>,
) -> crate::PingReport {
    run_statista_core(
        from_transport,
        to_generator,
        wait_time,
        StatSink::Events(events),
    )
    .await
}

pub async fn statista_with_collector(
    from_transport: Receiver<StatEntry>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
    collector: Sender<PingResult>,
) -> crate::PingReport {
    run_statista_core(
        from_transport,
        to_generator,
        wait_time,
        StatSink::Collector(collector),
    )
    .await
}

pub struct RttSequence {
    rtts: Vec<Duration>,
    sent: u64,
    received: u64,
}

impl Default for RttSequence {
    fn default() -> Self {
        Self::new()
    }
}

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

    #[test]
    fn entry_matching_exact_order() {
        let mut requests = VecDeque::new();
        let ts = Instant::now();
        requests.push_back(Entry { id: 0, ts });
        requests.push_back(Entry { id: 1, ts });

        let mut matched = Vec::new();

        let idx = 0;
        while let Some(req) = requests.pop_front() {
            match idx.cmp(&req.id) {
                Ordering::Greater => continue,
                Ordering::Equal => matched.push(idx),
                Ordering::Less => {
                    requests.push_front(req);
                }
            }
            break;
        }
        assert_eq!(matched, vec![0]);

        let idx = 1;
        while let Some(req) = requests.pop_front() {
            match idx.cmp(&req.id) {
                Ordering::Greater => continue,
                Ordering::Equal => matched.push(idx),
                Ordering::Less => {
                    requests.push_front(req);
                }
            }
            break;
        }
        assert_eq!(matched, vec![0, 1]);
        assert!(requests.is_empty());
    }

    #[test]
    fn entry_matching_less_puts_back() {
        let mut requests = VecDeque::new();
        let ts = Instant::now();
        requests.push_back(Entry { id: 5, ts });

        let idx = 3;
        while let Some(req) = requests.pop_front() {
            match idx.cmp(&req.id) {
                Ordering::Greater => continue,
                Ordering::Equal => {}
                Ordering::Less => {
                    requests.push_front(req);
                }
            }
            break;
        }
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, 5);
    }

    #[test]
    fn entry_matching_skips_lost_entries() {
        let mut requests = VecDeque::new();
        let ts = Instant::now();
        requests.push_back(Entry { id: 0, ts });
        requests.push_back(Entry { id: 1, ts });
        requests.push_back(Entry { id: 2, ts });

        let idx = 2;
        let mut skipped = Vec::new();
        while let Some(req) = requests.pop_front() {
            match idx.cmp(&req.id) {
                Ordering::Greater => {
                    skipped.push(req.id);
                    continue;
                }
                Ordering::Equal => {
                    break;
                }
                Ordering::Less => {
                    requests.push_front(req);
                    break;
                }
            }
        }
        assert_eq!(skipped, vec![0, 1]);
    }

    #[test]
    fn entry_matching_empty_queue() {
        let mut requests: VecDeque<Entry> = VecDeque::new();
        let _idx = 42u64;
        let result = requests.pop_front();
        assert!(result.is_none());
    }

    #[test]
    fn entry_matching_idempotent() {
        let mut requests = VecDeque::new();
        let ts = Instant::now();
        requests.push_back(Entry { id: 5, ts });

        let idx = 5;
        while let Some(req) = requests.pop_front() {
            match idx.cmp(&req.id) {
                Ordering::Greater => continue,
                Ordering::Equal => {}
                Ordering::Less => {
                    requests.push_front(req);
                }
            }
            break;
        }
        assert!(requests.is_empty());
    }

    #[tokio::test]
    async fn receive_timeout_removes_entry() {
        let entry = Entry {
            id: 42,
            ts: Instant::now(),
        };
        let req_mutex = Arc::new(Mutex::new(VecDeque::from(vec![entry])));

        receive_timeout(42, req_mutex.clone(), Duration::from_millis(1), None).await;

        let requests = req_mutex.lock().await;
        assert!(requests.is_empty());
    }

    #[tokio::test]
    async fn receive_timeout_removes_older_entries() {
        let req_mutex = Arc::new(Mutex::new(VecDeque::from(vec![
            Entry {
                id: 0,
                ts: Instant::now(),
            },
            Entry {
                id: 1,
                ts: Instant::now(),
            },
            Entry {
                id: 2,
                ts: Instant::now(),
            },
        ])));

        receive_timeout(1, req_mutex.clone(), Duration::from_millis(1), None).await;

        let requests = req_mutex.lock().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, 2);
    }

    #[tokio::test]
    async fn receive_timeout_removes_entries_up_to_index() {
        let req_mutex = Arc::new(Mutex::new(VecDeque::from(vec![
            Entry {
                id: 3,
                ts: Instant::now(),
            },
            Entry {
                id: 7,
                ts: Instant::now(),
            },
        ])));

        receive_timeout(5, req_mutex.clone(), Duration::from_millis(1), None).await;

        let requests = req_mutex.lock().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, 7);
    }

    #[tokio::test]
    async fn receive_timeout_signals_generator() {
        let entry = Entry {
            id: 0,
            ts: Instant::now(),
        };
        let req_mutex = Arc::new(Mutex::new(VecDeque::from(vec![entry])));
        let (gen_tx, mut gen_rx) = mpsc::channel(8);

        receive_timeout(0, req_mutex.clone(), Duration::from_millis(1), Some(gen_tx)).await;

        tokio::time::timeout(Duration::from_millis(100), gen_rx.recv())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn receive_timeout_empty_queue_no_panic() {
        let req_mutex = Arc::new(Mutex::new(VecDeque::new()));
        receive_timeout(0, req_mutex.clone(), Duration::from_millis(1), None).await;
        let requests = req_mutex.lock().await;
        assert!(requests.is_empty());
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
    async fn statista_ignores_close_before_open() {
        let (stat_tx, stat_rx) = mpsc::channel(8);
        let (result_tx, mut result_rx) = mpsc::channel(8);

        stat_tx
            .send(StatEntry::Close(crate::Response {
                id: 0,
                timestamp: Instant::now(),
                size: 0,
                ttl: None,
            }))
            .await
            .unwrap();
        stat_tx
            .send(StatEntry::Open(Entry {
                id: 0,
                ts: Instant::now(),
            }))
            .await
            .unwrap();
        drop(stat_tx);

        let report =
            statista_with_collector(stat_rx, None, Duration::from_secs(1), result_tx).await;
        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 0);
        assert!(result_rx.recv().await.is_none());
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
