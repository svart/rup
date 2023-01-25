use std::{time::Duration, collections::VecDeque, cmp::Ordering};

use tokio::sync::mpsc::{Receiver, Sender};

use crate::pinger::{PingReqResp, MsgType};

pub(crate) struct PingRTT {
    index: u64,
    rtt: Duration,
}

pub(crate) async fn statista(mut from_transport: Receiver<PingReqResp>,
                             to_presenter: Sender<PingRTT>) {
    let mut requests: VecDeque<PingReqResp> = VecDeque::new();

    loop {
        if let Some(resp) = from_transport.recv().await {
            match resp.t {
                MsgType::REQUEST => {
                    requests.push_back(resp);
                },
                MsgType::RESPONSE => {
                    let index = resp.index;

                    while let Some(req) = requests.pop_front() {
                        match index.cmp(&req.index) {
                            Ordering::Greater => continue,
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
