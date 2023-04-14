use std::sync::Arc;
use std::{collections::VecDeque, cmp::Ordering};
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::time::sleep;

use crate::pinger::{PingReqResp, MsgType};

#[derive(Debug)]
struct PingRTT {
    index: u64,
    rtt: Duration,
}

async fn receive_timeout(index: u64,
                         req_mutex: Arc<Mutex<VecDeque<PingReqResp>>>,
                         wait_time: Duration,
                         to_generator: Option<Sender<u8>>) {
    sleep(wait_time).await;

    let mut requests = req_mutex.lock().await;

    while let Some(req) = requests.front() {
        if req.index <= index {
            requests.pop_front();
            println!("seq: {index} request timeout");

            if let Some(gen_channel) = &to_generator {
                gen_channel.send(0).await.unwrap();
            }
        }
        else {
            break;
        }
    }
}

pub(crate) async fn statista(mut from_transport: Receiver<PingReqResp>,
                             to_generator: Option<Sender<u8>>,
                             wait_time: Duration) {
    let req_lock = Arc::new(Mutex::new(VecDeque::<PingReqResp>::new()));
    let (stat_pres_send, stat_pres_recv): (Sender<PingRTT>, Receiver<PingRTT>) = mpsc::channel(32);

    tokio::spawn(presenter(stat_pres_recv));

    loop {
        if let Some(resp) =  from_transport.recv().await {
            match resp.t {
                MsgType::Request => {
                    tokio::spawn(receive_timeout(resp.index, req_lock.clone(), wait_time, to_generator.clone()));

                    let mut requests = req_lock.lock().await;

                    requests.push_back(resp);
                },
                MsgType::Response => {
                    let index = resp.index;

                    let mut requests = req_lock.lock().await;

                    while let Some(req) = requests.pop_front() {
                        match index.cmp(&req.index) {
                            Ordering::Greater => {
                                println!("response reordering or lost");
                                continue;
                            },
                            Ordering::Equal => {
                                let timestamp = PingRTT {
                                    index,
                                    rtt: resp.timestamp.duration_since(req.timestamp)
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
                },
            }
        }
        else {
            return;
        }
    }
}

async fn presenter(mut from_statista: Receiver<PingRTT>) {
    while let Some(rtt) = from_statista.recv().await {
        println!("seq: {} rtt: {:#?}", rtt.index, rtt.rtt);
    }
}
