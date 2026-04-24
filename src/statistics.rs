use std::sync::Arc;
use std::time::Duration;
use std::{cmp::Ordering, collections::VecDeque};

use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::Mutex;
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

    tokio::spawn(presenter(stat_pres_recv));

    while let Some(resp) = from_transport.recv().await {
        match resp {
            StatEntry::Open(t) => {
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
}

async fn presenter(mut from_statista: Receiver<PingRTT>) {
    let mut sequence = RttSequence::new();

    while let Some(t) = from_statista.recv().await {
        println!("seq={} time={}", t.index, fmt_duration(t.rtt));
        sequence.record(t);
    }
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
        self.sent = self.sent.max(p.index + 1);
        self.received += 1;
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
    fn rtt_sequence_sent_tracking() {
        let mut seq = RttSequence::new();
        seq.record(PingRTT {
            index: 9,
            rtt: Duration::from_micros(50),
        });
        assert!(seq.sent >= 10);
        assert_eq!(seq.received, 1);
    }

    #[test]
    fn rtt_sequence_empty_stats_no_panic() {
        let mut seq = RttSequence::new();
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
}
