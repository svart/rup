use std::sync::Arc;
use std::{collections::VecDeque, cmp::Ordering};
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::sleep;

use crate::pinger::{PingReqResp, MsgType};

pub(crate) struct PingRTT {
    index: u64,
    rtt: Duration,
}

async fn receive_timeout(index: u64,
                         req_mutex: Arc<Mutex<VecDeque<PingReqResp>>>,
                         wait_time: Duration) {
    sleep(wait_time).await;

    let mut requests = req_mutex.lock().await;

    while let Some(req) = requests.front() {
        if req.index <= index {
            requests.pop_front();
            println!("request timeout: {index}");
        }
        else {
            break;
        }
    }
}

pub(crate) async fn statista(mut from_transport: Receiver<PingReqResp>,
                             to_presenter: Sender<PingRTT>,
                             wait_time: Duration) {
    let req_lock: Arc<Mutex<VecDeque<PingReqResp>>> = Arc::new(Mutex::new(VecDeque::new()));

    loop {
        if let Some(resp) = from_transport.recv().await {
            match resp.t {
                MsgType::REQUEST => {
                    tokio::spawn(receive_timeout(resp.index, req_lock.clone(), wait_time));

                    let mut requests = req_lock.lock().await;

                    requests.push_back(resp);
                },
                MsgType::RESPONSE => {
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
                                to_presenter.send(timestamp).await;
                            }
                            Ordering::Less => requests.push_front(req),
                        }
                        break;
                    }
                },
            }
        }
        else {
            break;
        }
    }
}

pub(crate) async fn presenter(mut from_statista: Receiver<PingRTT>) {
    loop {
        if let Some(rtt) = from_statista.recv().await {
            println!("Seq: {} rtt: {:#?}", rtt.index, rtt.rtt);
        }
        else {
            break;
        }
    }
}
