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
            println!("seq: {index} request timeout");

            if let Some(gen_channel) = &to_generator {
                gen_channel.send(()).await.unwrap();
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
    let (stat_pres_send, stat_pres_recv): (Sender<PingRTT>, Receiver<PingRTT>) = mpsc::channel(32);

    tokio::spawn(presenter(stat_pres_recv));

    // TODO: add another arm to listen on ctrl-c to exit immediately
    while let Some(resp) = from_transport.recv().await {
        match resp {
            StatEntry::Open(t) => {
                println!("statista: got request");
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
                println!("statista: got response");
                let index = t.id;

                let mut requests = req_lock.lock().await;

                while let Some(req) = requests.pop_front() {
                    match index.cmp(&req.id) {
                        Ordering::Greater => {
                            println!("seq: {index} response reordering or loss");
                            continue;
                        }
                        Ordering::Equal => {
                            let timestamp = PingRTT {
                                index,
                                rtt: t.ts.duration_since(req.ts),
                            };

                            if let Some(gen_channel) = &to_generator {
                                gen_channel.send(()).await.unwrap();
                            }

                            stat_pres_send.send(timestamp).await.expect("statista: should send request to presenter normally");
                        }
                        Ordering::Less => requests.push_front(req),
                    }
                    break;
                }
            }
        }
    }

    // TODO:
    // Here generator is finished.
    // Send signal to transport receiver to finish if there are no pending requests.
    // Otherwise wait till `requests` is empty and then send signal.
    println!("statista: finished receiving");
}

async fn presenter(mut from_statista: Receiver<PingRTT>) {
    let mut sequence = RttSequence::new();

    println!("presenter: started");
    while let Some(timestamp) = from_statista.recv().await {
        println!("seq: {} rtt: {:#?}", timestamp.index, timestamp.rtt);
        sequence.add(timestamp.rtt);
    }
    sequence.print_stats();
    println!("presenter: finished");
}

struct RttSequence(Vec<Duration>);

impl RttSequence {
    fn new() -> Self {
        RttSequence(Vec::with_capacity(1024))
    }

    fn add(&mut self, rtt: Duration) {
        self.0.push(rtt)
    }

    fn mean(&self) -> Duration {
        let avg = self.0.iter().sum::<Duration>().as_nanos() / self.0.len() as u128;
        Duration::from_nanos(u64::try_from(avg).unwrap())
    }

    fn std_deviation(&self) -> Duration {
        let avg = self.mean();

        let variance = self
            .0
            .iter()
            .map(|value| {
                let diff = avg.as_nanos().abs_diff((*value).as_nanos());
                diff * diff
            })
            .sum::<u128>() as f64
            / self.0.len() as f64;

        Duration::from_secs_f64(variance.sqrt() / 1_000_000_000.)
    }

    fn print_stats(&mut self) {
        if self.0.is_empty() {
            println!("no statistics collected");
            return;
        }

        self.0.sort();

        let min = self.0.iter().min().unwrap();
        let max = self.0.iter().max().unwrap();
        let avg = self.mean();
        let std_dev = self.std_deviation();
        let median = self.0.get(self.0.len() / 2).unwrap();

        println!("\nRTT statistics:");
        println!("min = {min:?}");
        println!("med = {median:?}");
        println!("avg = {avg:?}");
        println!("std_dev = {std_dev:?}");
        println!("max = {max:?}");
    }
}
