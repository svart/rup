use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) id: u64,
    pub(crate) request_size: Option<u16>,
    pub(crate) response_size: Option<u16>,
}

#[derive(Clone)]
pub(crate) struct Response {
    pub(crate) id: u64,
    pub(crate) timestamp: Instant,
}

pub(crate) struct Entry {
    pub(crate) id: u64,
    pub(crate) ts: Instant,
}

pub(crate) enum StatEntry {
    Open(Entry),
    Close(Entry),
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

pub const PING_HDR_LEN: usize = std::mem::size_of::<u64>()
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
        if let Some(n) = ping_number && id >= n {
            break;
        }

        let req = Request {
            id,
            request_size,
            response_size,
        };

        if to_tx_transport.send(req).await.is_err() {
            break;
        }

        match send_mode {
            SendMode::Adaptive(ref mut channel) => {
                let wait_for_response = channel.recv();
                tokio::select! {
                    r_val = wait_for_response => {
                        if r_val.is_none() {
                            break;
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
                    _ = sleep(Duration::from_millis(interval)) => {},
                    _ = &mut run_time => {
                        return;
                    }
                    _ = tokio::signal::ctrl_c() => {
                        return;
                    }
                }
            }
        }
        id += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_hdr_len_correct() {
        assert_eq!(PING_HDR_LEN, 12);
    }

    #[test]
    fn echo_serialization_roundtrip() {
        let echo = Echo {
            id: 42,
            len: 100,
            resp_size: 64,
        };
        let bytes = bincode::serialize(&echo).unwrap();
        let decoded: Echo = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded.id, 42);
        assert_eq!(decoded.len, 100);
        assert_eq!(decoded.resp_size, 64);
        assert_eq!(bytes.len(), PING_HDR_LEN);
    }

    #[test]
    fn request_roundtrip_through_channel() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let (tx, mut rx) = mpsc::channel(8);
            tx.send(Request {
                id: 7,
                request_size: Some(64),
                response_size: Some(32),
            })
            .await
            .unwrap();
            let req = rx.recv().await.unwrap();
            assert_eq!(req.id, 7);
            assert_eq!(req.request_size, Some(64));
        });
    }
}
