use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio::{sync::mpsc, time};

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) id: u64,
    pub(crate) request_size: Option<u16>,
    pub(crate) response_size: Option<u16>,
}

pub(crate) struct Response {
    pub(crate) id: u64,
    pub(crate) timestamp: Instant,
    pub(crate) size: usize,
}

pub(crate) struct Entry {
    pub(crate) id: u64,
    pub(crate) ts: Instant,
}

pub(crate) enum StatEntry {
    Open(Entry),
    Close(Entry)
}

pub(crate) enum SendMode {
    Adaptive(mpsc::Receiver<()>),
    Interval(u64),
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Echo {
    pub id: u64,
    pub len: u16,
    pub resp_size: u16,
}

// TODO: remove after TCP and ICMP are migrated to Transport trait
pub(crate) enum MsgType {
    Request,
    Response,
}

// TODO: remove after TCP and ICMP are migrated to Transport trait
pub(crate) struct PingReqResp {
    pub(crate) index: u64,
    pub(crate) timestamp: Instant,
    pub(crate) t: MsgType,
}

// sum of fields in Echo struct
pub const PING_HDR_LEN: usize = 0
    + std::mem::size_of::<u64>()
    + std::mem::size_of::<u16>()
    + std::mem::size_of::<u16>();

pub(crate) async fn generator(
    to_tx_transport: mpsc::Sender<Request>,
    mut send_mode: SendMode,
    ping_number: Option<u64>,
    run_time: Option<Duration>,
    request_size: Option<u16>,
    response_size: Option<u16>,
) {
    let mut id: u64 = 0;

    let run_time = run_time
        .map(|run_tune| sleep(run_tune))
        .unwrap_or(sleep(Duration::from_secs(u64::MAX)));
    tokio::pin!(run_time);

    loop {
        if let Some(n) = ping_number {
            if id >= n {
                println!("generator: all generated going out");
                break;
            }
        }

        let req = Request {
            id,
            request_size,
            response_size,
        };

        if let Err(err) = to_tx_transport.send(req).await {
            panic!("generator: Error during sending {id} to transport: {err}");
        }

        match send_mode {
            SendMode::Adaptive(ref mut channel) => {
                tokio::select! {
                    r_val = channel.recv() => {
                        if r_val.is_none() {
                            panic!("generator: cannot receive from transport");
                        }
                    }
                    _ = &mut run_time => {
                        return;
                    }
                    _ = tokio::signal::ctrl_c() => {
                        println!("generator: got signal, going out");
                        return;
                    }
                }
            }
            SendMode::Interval(interval) => {
                tokio::select! {
                    _ = time::sleep(Duration::from_millis(interval)) => {},
                    _ = &mut run_time => {
                        return;
                    }
                    _ = tokio::signal::ctrl_c() => {
                        println!("generator: got signal, going out");
                        return;
                    }
                }
            }
        }
        id += 1;
    }
}
