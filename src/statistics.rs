use std::sync::Arc;
use std::time::Duration;
use std::{cmp::Ordering, collections::VecDeque};

use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::{oneshot, Mutex};
use tokio::time::sleep;

use crate::pinger::{Entry, StatEntry};

#[derive(Debug)]
struct PingRTT {
    index: u64,
    rtt: Duration,
}

fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 1.0 {
        format!("{secs:.3} s")
    } else if secs >= 0.001 {
        format!("{:.3} ms", secs * 1000.0)
    } else {
        format!("{:.3} µs", secs * 1_000_000.0)
    }
}

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
            println!("seq={index} timeout");

            if let Some(gen_channel) = &to_generator {
                let _ = gen_channel.send(()).await;
            }
        } else {
            break;
        }
    }
}

pub(crate) async fn statista(
    mut from_transport: Receiver<StatEntry>,
    to_generator: Option<Sender<()>>,
    wait_time: Duration,
) {
    let req_lock = Arc::new(Mutex::new(VecDeque::<Entry>::new()));
    let (stat_pres_send, stat_pres_recv): (Sender<PingRTT>, Receiver<PingRTT>) =
        mpsc::channel(32);
    let (sent_tx, sent_rx) = oneshot::channel();

    tokio::spawn(presenter(stat_pres_recv, sent_rx));

    let mut total_sent = 0u64;

    while let Some(resp) = from_transport.recv().await {
        match resp {
            StatEntry::Open(t) => {
                total_sent += 1;
                tokio::spawn(receive_timeout(
                    t.id,
                    req_lock.clone(),
                    wait_time,
                    to_generator.clone(),
                ));

                let mut requests = req_lock.lock().await;
                requests.push_back(t);
            }
            StatEntry::Close(t) => {
                let index = t.id;

                let mut requests = req_lock.lock().await;

                while let Some(req) = requests.pop_front() {
                    match index.cmp(&req.id) {
                        Ordering::Greater => {
                            println!("seq={index} reorder or loss");
                            continue;
                        }
                        Ordering::Equal => {
                            let ping = PingRTT {
                                index,
                                rtt: t.ts.duration_since(req.ts),
                            };

                            if let Some(gen_channel) = &to_generator {
                                let _ = gen_channel.send(()).await;
                            }

                            if stat_pres_send.send(ping).await.is_err() {
                                return;
                            }
                        }
                        Ordering::Less => requests.push_front(req),
                    }
                    break;
                }
            }
        }
    }

    let _ = sent_tx.send(total_sent);
}

async fn presenter(
    mut from_statista: Receiver<PingRTT>,
    sent_rx: oneshot::Receiver<u64>,
) {
    let mut sequence = RttSequence::new();

    while let Some(t) = from_statista.recv().await {
        println!("seq={} time={}", t.index, fmt_duration(t.rtt));
        sequence.record(t);
    }

    let total_sent = sent_rx.await.unwrap_or(sequence.received);
    sequence.set_total_sent(total_sent);
    sequence.print_stats();
}

struct RttSequence {
    rtts: Vec<Duration>,
    sent: u64,
    received: u64,
}

impl RttSequence {
    fn new() -> Self {
        RttSequence {
            rtts: Vec::with_capacity(1024),
            sent: 0,
            received: 0,
        }
    }

    fn record(&mut self, p: PingRTT) {
        self.rtts.push(p.rtt);
        self.received += 1;
    }

    fn set_total_sent(&mut self, n: u64) {
        self.sent = n;
    }

    fn mean(&self) -> Duration {
        let avg =
            self.rtts.iter().sum::<Duration>().as_nanos() / self.rtts.len() as u128;
        Duration::from_nanos(u64::try_from(avg).unwrap_or(u64::MAX))
    }

    fn std_deviation(&self) -> Duration {
        let avg = self.mean();
        let variance = self
            .rtts
            .iter()
            .map(|value| {
                let diff = avg.as_nanos().abs_diff(value.as_nanos());
                diff * diff
            })
            .sum::<u128>() as f64
            / self.rtts.len() as f64;
        Duration::from_secs_f64(variance.sqrt() / 1_000_000_000.)
    }

    fn print_stats(&mut self) {
        if self.rtts.is_empty() {
            println!("no statistics collected");
            return;
        }

        self.rtts.sort();

        let loss_pct = if self.sent > 0 {
            (self.sent - self.received) as f64 / self.sent as f64 * 100.0
        } else {
            0.0
        };

        let min = self.rtts[0];
        let max = self.rtts[self.rtts.len() - 1];
        let avg = self.mean();
        let std_dev = self.std_deviation();
        let median = self.rtts[self.rtts.len() / 2];

        println!(
            "\n--- statistics ---\n\
             {sr} requests sent, {rc} received, {loss:.0}% loss\n\
             min/med/avg/max = {mi} / {me} / {av} / {ma}\n\
             std_dev = {sd}",
            sr = self.sent,
            rc = self.received,
            loss = loss_pct,
            mi = fmt_duration(min),
            me = fmt_duration(median),
            av = fmt_duration(avg),
            ma = fmt_duration(max),
            sd = fmt_duration(std_dev),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn rtt_sequence_mean() {
        let mut seq = RttSequence::new();
        for i in 0..4 {
            seq.record(PingRTT {
                index: i,
                rtt: Duration::from_micros(100 * (i as u64 + 1)),
            });
        }
        assert_eq!(seq.mean(), Duration::from_micros(250));
    }

    #[test]
    fn rtt_sequence_mean_single_entry() {
        let mut seq = RttSequence::new();
        seq.record(PingRTT { index: 0, rtt: Duration::from_micros(42) });
        assert_eq!(seq.mean(), Duration::from_micros(42));
    }

    #[test]
    fn rtt_sequence_mean_large_values() {
        let mut seq = RttSequence::new();
        seq.record(PingRTT { index: 0, rtt: Duration::from_secs(10) });
        seq.record(PingRTT { index: 1, rtt: Duration::from_secs(20) });
        assert_eq!(seq.mean(), Duration::from_secs(15));
    }

    #[test]
    fn rtt_sequence_median_odd() {
        let mut seq = RttSequence::new();
        for i in 0..5 {
            seq.record(PingRTT { index: i, rtt: Duration::from_micros((i as u64 + 1) * 10) });
        }
        seq.set_total_sent(5);
        seq.rtts.sort();
        assert_eq!(seq.rtts[seq.rtts.len() / 2], Duration::from_micros(30));
    }

    #[test]
    fn rtt_sequence_median_even() {
        let mut seq = RttSequence::new();
        for i in 0..4 {
            seq.record(PingRTT { index: i, rtt: Duration::from_micros((i as u64 + 1) * 10) });
        }
        seq.set_total_sent(4);
        seq.rtts.sort();
        assert_eq!(seq.rtts[seq.rtts.len() / 2], Duration::from_micros(30));
    }

    #[test]
    fn rtt_sequence_single_entry() {
        let mut seq = RttSequence::new();
        seq.record(PingRTT { index: 0, rtt: Duration::from_micros(100) });
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
            seq.record(PingRTT { index: i as u64, rtt: Duration::from_nanos(i) });
        }
        seq.set_total_sent(1000);
        assert_eq!(seq.received, 1000);
        assert!(!seq.rtts.is_empty());
    }

    #[test]
    fn rtt_sequence_std_dev_zero() {
        let mut seq = RttSequence::new();
        for i in 0..3 {
            seq.record(PingRTT { index: i, rtt: Duration::from_micros(100) });
        }
        assert_eq!(seq.std_deviation(), Duration::from_nanos(0));
    }

    #[test]
    fn rtt_sequence_std_dev_known() {
        let mut seq = RttSequence::new();
        seq.record(PingRTT { index: 0, rtt: Duration::from_micros(0) });
        seq.record(PingRTT { index: 1, rtt: Duration::from_micros(100) });
        let std_dev = seq.std_deviation();
        assert!(std_dev.as_micros() > 0);
    }

    #[test]
    fn rtt_sequence_exact_sent_tracking() {
        let mut seq = RttSequence::new();
        seq.record(PingRTT {
            index: 9,
            rtt: Duration::from_micros(50),
        });
        seq.set_total_sent(10);
        assert_eq!(seq.sent, 10);
        assert_eq!(seq.received, 1);
    }

    #[test]
    fn rtt_sequence_empty_stats_no_panic() {
        let mut seq = RttSequence::new();
        seq.print_stats();
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
        seq.record(PingRTT { index: 0, rtt: Duration::from_micros(1) });
        seq.record(PingRTT { index: 1, rtt: Duration::from_micros(2) });
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
    fn rtt_sequence_print_stats_happy_path() {
        let mut seq = RttSequence::new();
        seq.record(PingRTT { index: 0, rtt: Duration::from_micros(100) });
        seq.record(PingRTT { index: 1, rtt: Duration::from_micros(200) });
        seq.set_total_sent(2);
        seq.print_stats();
    }

    #[test]
    fn fmt_duration_micros() {
        let s = fmt_duration(Duration::from_micros(50));
        assert!(s.contains("µs"));
    }

    #[test]
    fn fmt_duration_millis() {
        let s = fmt_duration(Duration::from_millis(5));
        assert!(s.contains("ms"));
    }

    #[test]
    fn fmt_duration_secs() {
        let s = fmt_duration(Duration::from_secs(2));
        assert!(s.contains("s"));
    }

    #[test]
    fn fmt_duration_exact_boundaries() {
        assert!(fmt_duration(Duration::from_millis(1)).contains("ms"));
        assert!(fmt_duration(Duration::from_secs(1)).contains("s"));
        assert!(fmt_duration(Duration::from_micros(999)).contains("µs"));
        assert!(fmt_duration(Duration::from_millis(999)).contains("ms"));
    }

    #[test]
    fn fmt_duration_zero() {
        let s = fmt_duration(Duration::from_nanos(0));
        assert!(s.contains("µs"));
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
                Ordering::Less => { requests.push_front(req); }
            }
            break;
        }
        assert_eq!(matched, vec![0]);

        let idx = 1;
        while let Some(req) = requests.pop_front() {
            match idx.cmp(&req.id) {
                Ordering::Greater => continue,
                Ordering::Equal => matched.push(idx),
                Ordering::Less => { requests.push_front(req); }
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
                Ordering::Less => { requests.push_front(req); }
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
        loop {
            match requests.pop_front() {
                Some(req) => match idx.cmp(&req.id) {
                    Ordering::Greater => { skipped.push(req.id); continue; }
                    Ordering::Equal => { break; }
                    Ordering::Less => { requests.push_front(req); break; }
                },
                None => break,
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
                Ordering::Less => { requests.push_front(req); }
            }
            break;
        }
        assert!(requests.is_empty());
    }

    #[tokio::test]
    async fn receive_timeout_removes_entry() {
        let entry = Entry { id: 42, ts: Instant::now() };
        let req_mutex = Arc::new(Mutex::new(VecDeque::from(vec![entry])));

        receive_timeout(42, req_mutex.clone(), Duration::from_millis(1), None).await;

        let requests = req_mutex.lock().await;
        assert!(requests.is_empty());
    }

    #[tokio::test]
    async fn receive_timeout_removes_older_entries() {
        let req_mutex = Arc::new(Mutex::new(VecDeque::from(vec![
            Entry { id: 0, ts: Instant::now() },
            Entry { id: 1, ts: Instant::now() },
            Entry { id: 2, ts: Instant::now() },
        ])));

        receive_timeout(1, req_mutex.clone(), Duration::from_millis(1), None).await;

        let requests = req_mutex.lock().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, 2);
    }

    #[tokio::test]
    async fn receive_timeout_removes_entries_up_to_index() {
        let req_mutex = Arc::new(Mutex::new(VecDeque::from(vec![
            Entry { id: 3, ts: Instant::now() },
            Entry { id: 7, ts: Instant::now() },
        ])));

        receive_timeout(5, req_mutex.clone(), Duration::from_millis(1), None).await;

        let requests = req_mutex.lock().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].id, 7);
    }

    #[tokio::test]
    async fn receive_timeout_signals_generator() {
        let entry = Entry { id: 0, ts: Instant::now() };
        let req_mutex = Arc::new(Mutex::new(VecDeque::from(vec![entry])));
        let (gen_tx, mut gen_rx) = mpsc::channel(8);

        receive_timeout(0, req_mutex.clone(), Duration::from_millis(1), Some(gen_tx)).await;

        let signal = tokio::time::timeout(Duration::from_millis(100), gen_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(signal, ());
    }

    #[tokio::test]
    async fn receive_timeout_empty_queue_no_panic() {
        let req_mutex = Arc::new(Mutex::new(VecDeque::new()));
        receive_timeout(0, req_mutex.clone(), Duration::from_millis(1), None).await;
        let requests = req_mutex.lock().await;
        assert!(requests.is_empty());
    }
}
