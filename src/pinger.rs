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

#[derive(Clone, Debug)]
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
    fn echo_zero_values() {
        let echo = Echo { id: 0, len: 0, resp_size: 0 };
        let bytes = bincode::serialize(&echo).unwrap();
        assert_eq!(bytes.len(), PING_HDR_LEN);
        let decoded: Echo = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded.id, 0);
        assert_eq!(decoded.len, 0);
        assert_eq!(decoded.resp_size, 0);
    }

    #[test]
    fn echo_max_values() {
        let echo = Echo { id: u64::MAX, len: u16::MAX, resp_size: u16::MAX };
        let bytes = bincode::serialize(&echo).unwrap();
        assert_eq!(bytes.len(), PING_HDR_LEN);
        let decoded: Echo = bincode::deserialize(&bytes).unwrap();
        assert_eq!(decoded.id, u64::MAX);
        assert_eq!(decoded.len, u16::MAX);
        assert_eq!(decoded.resp_size, u16::MAX);
    }

    #[test]
    fn echo_field_order_guaranteed() {
        let echo = Echo { id: 1, len: 2, resp_size: 3 };
        let bytes = bincode::serialize(&echo).unwrap();
        assert_eq!(bytes[0..8], 1u64.to_le_bytes());
        assert_eq!(bytes[8..10], 2u16.to_le_bytes());
        assert_eq!(bytes[10..12], 3u16.to_le_bytes());
    }

    #[test]
    fn echo_deserialize_invalid_too_short() {
        let result: Result<Echo, _> = bincode::deserialize(&[0u8; 4]);
        assert!(result.is_err());
    }

    #[test]
    fn echo_deserialize_extra_bytes_ignored() {
        let result: Result<Echo, _> = bincode::deserialize(&[0u8; 20]);
        assert!(result.is_ok());
        let echo = result.unwrap();
        assert_eq!(echo.id, 0);
        assert_eq!(echo.len, 0);
        assert_eq!(echo.resp_size, 0);
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

    #[test]
    fn request_default_sizes() {
        let req = Request { id: 0, request_size: None, response_size: None };
        assert!(req.request_size.is_none());
        assert!(req.response_size.is_none());
    }

    #[tokio::test]
    async fn generator_incrementing_ids() {
        let (tx, mut rx) = mpsc::channel(8);

        tokio::spawn(generator(
            tx,
            SendMode::Interval(1),
            Some(3),
            None,
            None,
            None,
        ));

        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(rx.recv().await.unwrap().id);
        }
        assert_eq!(ids, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn generator_stops_at_ping_number() {
        let (tx, mut rx) = mpsc::channel(8);

        let handle = tokio::spawn(generator(
            tx,
            SendMode::Interval(1),
            Some(5),
            None,
            None,
            None,
        ));

        let mut count = 0;
        while let Some(_) = rx.recv().await {
            count += 1;
            if count >= 5 { break; }
        }
        assert_eq!(count, 5);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn generator_sends_request_sizes() {
        let (tx, mut rx) = mpsc::channel(8);

        tokio::spawn(generator(
            tx,
            SendMode::Interval(1),
            Some(1),
            None,
            Some(100),
            Some(200),
        ));

        let req = rx.recv().await.unwrap();
        assert_eq!(req.request_size, Some(100));
        assert_eq!(req.response_size, Some(200));
    }

    #[tokio::test]
    async fn generator_adaptive_mode() {
        let (tx, mut rx) = mpsc::channel(8);
        let (signal_tx, signal_rx) = mpsc::channel(8);

        tokio::spawn(generator(
            tx,
            SendMode::Adaptive(signal_rx),
            Some(3),
            None,
            None,
            None,
        ));

        let req1 = rx.recv().await.unwrap();
        assert_eq!(req1.id, 0);
        signal_tx.send(()).await.unwrap();

        let req2 = rx.recv().await.unwrap();
        assert_eq!(req2.id, 1);
        signal_tx.send(()).await.unwrap();

        let req3 = rx.recv().await.unwrap();
        assert_eq!(req3.id, 2);
    }

    #[tokio::test]
    async fn generator_exits_on_channel_close() {
        let (tx, rx) = mpsc::channel(8);

        let handle = tokio::spawn(generator(
            tx,
            SendMode::Interval(1000),
            Some(100),
            None,
            None,
            None,
        ));

        drop(rx);

        tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn generator_adaptive_stops_on_channel_close() {
        let (tx, mut rx) = mpsc::channel(8);
        let (signal_tx, signal_rx) = mpsc::channel::<()>(8);

        let handle = tokio::spawn(generator(
            tx,
            SendMode::Adaptive(signal_rx),
            Some(100),
            None,
            None,
            None,
        ));

        let req = rx.recv().await.unwrap();
        assert_eq!(req.id, 0);
        drop(signal_tx);

        tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn generator_adaptive_exits_on_channel_close() {
        let (tx, rx) = mpsc::channel(8);
        let (_signal_tx, signal_rx) = mpsc::channel::<()>(8);

        let handle = tokio::spawn(generator(
            tx,
            SendMode::Adaptive(signal_rx),
            Some(100),
            None,
            None,
            None,
        ));

        drop(rx);

        tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn send_mode_interval_creation() {
        match SendMode::Interval(500) {
            SendMode::Interval(v) => assert_eq!(v, 500),
            _ => panic!("expected Interval"),
        }
    }
}
