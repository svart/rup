use std::sync::Arc;
use std::time::Duration;
use std::{cmp::Ordering, collections::VecDeque};

use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::pinger::{MsgType, PingReqResp};

#[derive(Debug)]
struct PingRTT {
    index: u64,
    rtt: Duration,
}

async fn receive_timeout(
    index: u64,
    req_mutex: Arc<Mutex<VecDeque<PingReqResp>>>,
    wait_time: Duration,
    to_generator: Option<Sender<u8>>,
) {
    sleep(wait_time).await;

    let mut requests = req_mutex.lock().await;

    while let Some(req) = requests.front() {
        if req.index <= index {
            requests.pop_front();
            println!("seq: {index} request timeout");

            if let Some(gen_channel) = &to_generator {
                gen_channel.send(0).await.unwrap();
            }
        } else {
            break;
        }
    }
}

pub(crate) async fn statista(
    mut from_transport: Receiver<PingReqResp>,
    to_generator: Option<Sender<u8>>,
    wait_time: Duration,
) {
    let req_lock = Arc::new(Mutex::new(VecDeque::<PingReqResp>::new()));
    let (stat_pres_send, stat_pres_recv): (Sender<PingRTT>, Receiver<PingRTT>) = mpsc::channel(32);

    tokio::spawn(presenter(stat_pres_recv));

    loop {
        if let Some(resp) = from_transport.recv().await {
            match resp.t {
                MsgType::Request => {
                    tokio::spawn(receive_timeout(
                        resp.index,
                        req_lock.clone(),
                        wait_time,
                        to_generator.clone(),
                    ));

                    let mut requests = req_lock.lock().await;

                    requests.push_back(resp);
                }
                MsgType::Response => {
                    let index = resp.index;

                    let mut requests = req_lock.lock().await;

                    while let Some(req) = requests.pop_front() {
                        match index.cmp(&req.index) {
                            Ordering::Greater => {
                                println!("seq: {index} response reordering or loss");
                                continue;
                            }
                            Ordering::Equal => {
                                let timestamp = PingRTT {
                                    index,
                                    rtt: resp.timestamp.duration_since(req.timestamp),
                                };

                                if let Some(gen_channel) = &to_generator {
                                    gen_channel.send(0).await.unwrap();
                                }

                                stat_pres_send.send(timestamp).await.unwrap();
                            }
                            Ordering::Less => requests.push_front(req),
                        }
                        break;
                    }
                }
            }
        } else {
            return;
        }
    }
}

async fn presenter(mut from_statista: Receiver<PingRTT>) {
    let mut sequence = RttSequence::new();

    while let Some(timestamp) = from_statista.recv().await {
        println!("seq: {} rtt: {:#?}", timestamp.index, timestamp.rtt);
        sequence.add(timestamp.rtt);
    }
    sequence.print_stats();
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

        let variance = self.0
            .iter()
            .map(|value| {
                let diff = avg.as_nanos() - (*value).as_nanos();
                diff * diff
            })
            .sum::<u128>() as f64 / self.0.len() as f64;

        Duration::from_secs_f64(variance.sqrt() / 1_000_000_000.)
    }

    fn print_stats(&self) {
        if self.0.is_empty() {
            println!("no statistics collected");
            return;
        }

        let min = self.0.iter().min().unwrap();
        let max = self.0.iter().max().unwrap();
        let avg = self.mean();
        let std_dev = self.std_deviation();

        println!("rtt min/avg/max/std_dev = {min:?}/{avg:?}/{max:?}/{std_dev:?}");
    }
}
