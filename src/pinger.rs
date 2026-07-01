use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::{Instant as TokioInstant, sleep, sleep_until};

pub const PING_HDR_LEN: usize =
    std::mem::size_of::<u64>() + std::mem::size_of::<u16>() + std::mem::size_of::<u16>();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketSize(u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacketSizeError {
    value: u16,
}

impl PacketSize {
    pub const MIN: u16 = PING_HDR_LEN as u16;

    pub fn new(value: u16) -> Result<Self, PacketSizeError> {
        if value >= Self::MIN {
            Ok(Self(value))
        } else {
            Err(PacketSizeError { value })
        }
    }

    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for PacketSize {
    type Error = PacketSizeError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for PacketSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "packet size {} is smaller than {}",
            self.value,
            PacketSize::MIN
        )
    }
}

impl std::error::Error for PacketSizeError {}

#[derive(Clone, Debug)]
pub struct Request {
    pub id: u64,
    pub request_size: Option<PacketSize>,
    pub response_size: Option<PacketSize>,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub id: u64,
    pub timestamp: Instant,
    pub size: usize,
    pub ttl: Option<u8>,
}

pub struct Entry {
    pub id: u64,
    pub ts: Instant,
}

pub enum StatEntry {
    Open(Entry),
    Close(Response),
}

pub enum SendMode {
    Adaptive(mpsc::Receiver<()>),
    Interval(Duration),
}

pub struct GeneratorConfig {
    pub send_mode: SendMode,
    pub ping_number: Option<u64>,
    pub run_time: Option<Duration>,
    pub request_size: Option<PacketSize>,
    pub response_size: Option<PacketSize>,
}

impl GeneratorConfig {
    pub fn new(send_mode: SendMode) -> Self {
        Self {
            send_mode,
            ping_number: None,
            run_time: None,
            request_size: None,
            response_size: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Echo {
    pub id: u64,
    pub len: u16,
    pub resp_size: u16,
}

pub async fn generator(to_tx_transport: mpsc::Sender<Request>, mut config: GeneratorConfig) {
    let mut id: u64 = 0;
    let stop_at = config
        .run_time
        .map(|run_time| TokioInstant::now() + run_time);

    loop {
        if let Some(n) = config.ping_number
            && id >= n
        {
            break;
        }

        let req = Request {
            id,
            request_size: config.request_size,
            response_size: config.response_size,
        };

        if to_tx_transport.send(req).await.is_err() {
            break;
        }

        if !wait_for_next_request(&mut config.send_mode, stop_at).await {
            return;
        }
        id += 1;
    }
}

async fn wait_for_next_request(send_mode: &mut SendMode, stop_at: Option<TokioInstant>) -> bool {
    match send_mode {
        SendMode::Adaptive(channel) => {
            if let Some(stop_at) = stop_at {
                tokio::select! {
                    signal = channel.recv() => signal.is_some(),
                    _ = sleep_until(stop_at) => false,
                }
            } else {
                channel.recv().await.is_some()
            }
        }
        SendMode::Interval(interval) => {
            if let Some(stop_at) = stop_at {
                tokio::select! {
                    _ = sleep(*interval) => true,
                    _ = sleep_until(stop_at) => false,
                }
            } else {
                sleep(*interval).await;
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::echo_codec;

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
        let bytes = echo_codec::encode_echo(&echo, PING_HDR_LEN);
        let decoded = echo_codec::decode_header(&bytes).unwrap();
        assert_eq!(decoded.id, 42);
        assert_eq!(decoded.len, 100);
        assert_eq!(decoded.resp_size, 64);
        assert_eq!(bytes.len(), PING_HDR_LEN);
    }

    #[test]
    fn echo_zero_values() {
        let echo = Echo {
            id: 0,
            len: 0,
            resp_size: 0,
        };
        let bytes = echo_codec::encode_echo(&echo, PING_HDR_LEN);
        assert_eq!(bytes.len(), PING_HDR_LEN);
        let decoded = echo_codec::decode_header(&bytes).unwrap();
        assert_eq!(decoded.id, 0);
        assert_eq!(decoded.len, 0);
        assert_eq!(decoded.resp_size, 0);
    }

    #[test]
    fn echo_max_values() {
        let echo = Echo {
            id: u64::MAX,
            len: u16::MAX,
            resp_size: u16::MAX,
        };
        let bytes = echo_codec::encode_echo(&echo, PING_HDR_LEN);
        assert_eq!(bytes.len(), PING_HDR_LEN);
        let decoded = echo_codec::decode_header(&bytes).unwrap();
        assert_eq!(decoded.id, u64::MAX);
        assert_eq!(decoded.len, u16::MAX);
        assert_eq!(decoded.resp_size, u16::MAX);
    }

    #[test]
    fn echo_field_order_guaranteed() {
        let echo = Echo {
            id: 1,
            len: 2,
            resp_size: 3,
        };
        let bytes = echo_codec::encode_echo(&echo, PING_HDR_LEN);
        assert_eq!(bytes[0..8], 1u64.to_le_bytes());
        assert_eq!(bytes[8..10], 2u16.to_le_bytes());
        assert_eq!(bytes[10..12], 3u16.to_le_bytes());
    }

    #[test]
    fn echo_deserialize_invalid_too_short() {
        let result = echo_codec::decode_header(&[0u8; 4]);
        assert!(result.is_err());
    }

    #[test]
    fn echo_deserialize_extra_bytes_ignored() {
        let result = echo_codec::decode_header(&[0u8; 20]);
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
                request_size: Some(PacketSize::new(64).unwrap()),
                response_size: Some(PacketSize::new(32).unwrap()),
            })
            .await
            .unwrap();
            let req = rx.recv().await.unwrap();
            assert_eq!(req.id, 7);
            assert_eq!(req.request_size, Some(PacketSize::new(64).unwrap()));
        });
    }

    #[test]
    fn request_default_sizes() {
        let req = Request {
            id: 0,
            request_size: None,
            response_size: None,
        };
        assert!(req.request_size.is_none());
        assert!(req.response_size.is_none());
    }

    #[tokio::test]
    async fn generator_incrementing_ids() {
        let (tx, mut rx) = mpsc::channel(8);

        tokio::spawn(generator(
            tx,
            GeneratorConfig {
                send_mode: SendMode::Interval(Duration::from_millis(1)),
                ping_number: Some(3),
                run_time: None,
                request_size: None,
                response_size: None,
            },
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
            GeneratorConfig {
                send_mode: SendMode::Interval(Duration::from_millis(1)),
                ping_number: Some(5),
                run_time: None,
                request_size: None,
                response_size: None,
            },
        ));

        let mut count = 0;
        while rx.recv().await.is_some() {
            count += 1;
            if count >= 5 {
                break;
            }
        }
        assert_eq!(count, 5);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn generator_sends_request_sizes() {
        let (tx, mut rx) = mpsc::channel(8);

        tokio::spawn(generator(
            tx,
            GeneratorConfig {
                send_mode: SendMode::Interval(Duration::from_millis(1)),
                ping_number: Some(1),
                run_time: None,
                request_size: Some(PacketSize::new(100).unwrap()),
                response_size: Some(PacketSize::new(200).unwrap()),
            },
        ));

        let req = rx.recv().await.unwrap();
        assert_eq!(req.request_size, Some(PacketSize::new(100).unwrap()));
        assert_eq!(req.response_size, Some(PacketSize::new(200).unwrap()));
    }

    #[tokio::test]
    async fn generator_adaptive_mode() {
        let (tx, mut rx) = mpsc::channel(8);
        let (signal_tx, signal_rx) = mpsc::channel(8);

        tokio::spawn(generator(
            tx,
            GeneratorConfig {
                send_mode: SendMode::Adaptive(signal_rx),
                ping_number: Some(3),
                run_time: None,
                request_size: None,
                response_size: None,
            },
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
            GeneratorConfig {
                send_mode: SendMode::Interval(Duration::from_millis(1000)),
                ping_number: Some(100),
                run_time: None,
                request_size: None,
                response_size: None,
            },
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
            GeneratorConfig {
                send_mode: SendMode::Adaptive(signal_rx),
                ping_number: Some(100),
                run_time: None,
                request_size: None,
                response_size: None,
            },
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
            GeneratorConfig {
                send_mode: SendMode::Adaptive(signal_rx),
                ping_number: Some(100),
                run_time: None,
                request_size: None,
                response_size: None,
            },
        ));

        drop(rx);

        tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn send_mode_interval_creation() {
        match SendMode::Interval(Duration::from_millis(500)) {
            SendMode::Interval(v) => assert_eq!(v, Duration::from_millis(500)),
            _ => panic!("expected Interval"),
        }
    }
}
