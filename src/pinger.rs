use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio::{sync::mpsc, time};

#[derive(Clone, Debug)]
pub(crate) enum MsgType {
    Request,
    Response,
}

#[derive(Clone, Debug)]
pub(crate) struct PingReqResp {
    pub(crate) index: u64,
    pub(crate) timestamp: Instant,
    pub(crate) t: MsgType,
}

pub(crate) enum SendMode {
    Adaptive(mpsc::Receiver<u8>),
    Interval(u64),
}

pub const PING_HDR_LEN: usize = 8 + 2 + 2;

#[derive(Serialize, Deserialize)]
pub(crate) struct Echo {
    pub id: u64,
    pub len: u16,
    pub resp_size: u16,
}

pub(crate) async fn generator(
    to_tx_transport: mpsc::Sender<PingReqResp>,
    mut send_mode: SendMode,
    ping_number: Option<u64>,
    run_time: Option<Duration>,
) {
    let mut i: u64 = 0;
    let run_time = run_time
        .map(|run_tune| sleep(run_tune))
        .unwrap_or(sleep(Duration::from_secs(u64::MAX)));
    tokio::pin!(run_time);
    loop {
        if let Some(n) = ping_number {
            if i >= n {
                break;
            }
        }

        let req = PingReqResp {
            index: i,
            timestamp: Instant::now(),
            t: MsgType::Request,
        };

        if let Err(err) = to_tx_transport.send(req).await {
            panic!("client: Error during sending {i} to transport: {err}");
        }

        match &mut send_mode {
            SendMode::Adaptive(channel) => {
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
                        return;
                    }
                }
            }
            SendMode::Interval(interval) => {
                tokio::select! {
                    _ = time::sleep(Duration::from_millis(*interval)) => {},
                    _ = &mut run_time => {
                        return;
                    }
                    _ = tokio::signal::ctrl_c() => {
                        return;
                    }
                }
            }
        }
        i += 1;
    }
}
