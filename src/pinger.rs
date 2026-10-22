use std::fmt;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::{
    Instant as TokioInstant, Interval, MissedTickBehavior, interval_at, sleep_until,
};

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

pub struct Echo {
    pub id: u64,
    pub len: u16,
    pub resp_size: u16,
}

enum Pacer {
    Adaptive(mpsc::Receiver<()>),
    Interval(Interval),
}

impl Pacer {
    fn new(send_mode: SendMode) -> Self {
        match send_mode {
            SendMode::Adaptive(channel) => Self::Adaptive(channel),
            SendMode::Interval(period) => {
                // Fixed deadlines keep send overhead and timer rounding from
                // adding up; Delay avoids a catch-up burst after a long stall.
                let mut ticker = interval_at(TokioInstant::now() + period, period);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
                Self::Interval(ticker)
            }
        }
    }
}

pub(crate) async fn generator(to_tx_transport: mpsc::Sender<Request>, config: GeneratorConfig) {
    let mut id: u64 = 0;
    let stop_at = config
        .run_time
        .map(|run_time| TokioInstant::now() + run_time);
    let mut pacer = Pacer::new(config.send_mode);

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

        id += 1;
        if config.ping_number.is_some_and(|n| id >= n) {
            break;
        }

        if !wait_for_next_request(&mut pacer, stop_at).await {
            return;
        }
    }
}

async fn wait_for_next_request(pacer: &mut Pacer, stop_at: Option<TokioInstant>) -> bool {
    match pacer {
        Pacer::Adaptive(channel) => {
            if let Some(stop_at) = stop_at {
                tokio::select! {
                    signal = channel.recv() => signal.is_some(),
                    _ = sleep_until(stop_at) => false,
                }
            } else {
                channel.recv().await.is_some()
            }
        }
        Pacer::Interval(ticker) => {
            if let Some(stop_at) = stop_at {
                // A tick due exactly at the run-time end must not send.
                tokio::select! {
                    biased;
                    _ = sleep_until(stop_at) => false,
                    _ = ticker.tick() => true,
                }
            } else {
                ticker.tick().await;
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(send_mode: SendMode, ping_number: Option<u64>) -> GeneratorConfig {
        GeneratorConfig {
            send_mode,
            ping_number,
            run_time: None,
            request_size: None,
            response_size: None,
        }
    }

    fn every_ms(ms: u64) -> SendMode {
        SendMode::Interval(Duration::from_millis(ms))
    }

    async fn collect_ids(mut rx: mpsc::Receiver<Request>) -> Vec<u64> {
        let mut ids = Vec::new();
        while let Some(req) = rx.recv().await {
            ids.push(req.id);
        }
        ids
    }

    #[test]
    fn packet_size_rejects_values_below_header_length() {
        assert!(PacketSize::new(PacketSize::MIN - 1).is_err());
        assert_eq!(PacketSize::new(PacketSize::MIN).unwrap().get(), 12);
        assert_eq!(
            PacketSize::try_from(5).unwrap_err().to_string(),
            "packet size 5 is smaller than 12"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn generator_sends_incrementing_ids_and_stops_at_ping_number() {
        let (tx, rx) = mpsc::channel(8);
        let handle = tokio::spawn(generator(tx, config(every_ms(1), Some(5))));

        assert_eq!(collect_ids(rx).await, vec![0, 1, 2, 3, 4]);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn generator_does_not_wait_after_last_request() {
        let (tx, mut rx) = mpsc::channel(1);

        let handle = tokio::spawn(generator(
            tx,
            config(SendMode::Interval(Duration::from_secs(60)), Some(1)),
        ));

        assert_eq!(rx.recv().await.unwrap().id, 0);
        tokio::time::timeout(Duration::from_millis(50), handle)
            .await
            .expect("generator should finish immediately after its final request")
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn generator_interval_does_not_accumulate_timer_overhead() {
        // Tokio rounds timer deadlines up to whole milliseconds, so a 1.5 ms
        // interval exposes a schedule that restarts from each late wakeup.
        let interval = Duration::from_micros(1500);
        let (tx, mut rx) = mpsc::channel(16);
        let started = TokioInstant::now();

        tokio::spawn(generator(
            tx,
            config(SendMode::Interval(interval), Some(11)),
        ));

        let mut last_sent = started;
        while rx.recv().await.is_some() {
            last_sent = TokioInstant::now();
        }

        let elapsed = last_sent - started;
        assert!(
            elapsed <= interval * 10 + Duration::from_millis(1),
            "10 intervals took {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn generator_does_not_send_a_request_due_at_run_time_end() {
        // select! polls ready branches in random order, so repeat the race.
        for _ in 0..32 {
            let (tx, rx) = mpsc::channel(16);

            tokio::spawn(generator(
                tx,
                GeneratorConfig {
                    run_time: Some(Duration::from_secs(1)),
                    ..config(every_ms(100), None)
                },
            ));

            assert_eq!(collect_ids(rx).await.len(), 10);
        }
    }

    #[tokio::test]
    async fn generator_sends_request_sizes() {
        let (tx, mut rx) = mpsc::channel(8);

        tokio::spawn(generator(
            tx,
            GeneratorConfig {
                request_size: Some(PacketSize::new(100).unwrap()),
                response_size: Some(PacketSize::new(200).unwrap()),
                ..config(every_ms(1), Some(1))
            },
        ));

        let req = rx.recv().await.unwrap();
        assert_eq!(req.request_size, Some(PacketSize::new(100).unwrap()));
        assert_eq!(req.response_size, Some(PacketSize::new(200).unwrap()));
    }

    #[tokio::test]
    async fn generator_adaptive_mode_sends_one_request_per_signal() {
        let (tx, mut rx) = mpsc::channel(8);
        let (signal_tx, signal_rx) = mpsc::channel(8);

        tokio::spawn(generator(
            tx,
            config(SendMode::Adaptive(signal_rx), Some(3)),
        ));

        assert_eq!(rx.recv().await.unwrap().id, 0);
        assert!(rx.try_recv().is_err(), "second request sent before signal");
        signal_tx.send(()).await.unwrap();
        assert_eq!(rx.recv().await.unwrap().id, 1);
        signal_tx.send(()).await.unwrap();
        assert_eq!(rx.recv().await.unwrap().id, 2);
    }

    #[tokio::test]
    async fn generator_exits_when_request_receiver_dropped() {
        let (_signal_tx, signal_rx) = mpsc::channel(8);
        for send_mode in [every_ms(1000), SendMode::Adaptive(signal_rx)] {
            let (tx, rx) = mpsc::channel(8);
            let handle = tokio::spawn(generator(tx, config(send_mode, Some(100))));

            drop(rx);

            tokio::time::timeout(Duration::from_millis(200), handle)
                .await
                .unwrap()
                .unwrap();
        }
    }

    #[tokio::test]
    async fn generator_adaptive_exits_when_signal_sender_dropped() {
        let (tx, mut rx) = mpsc::channel(8);
        let (signal_tx, signal_rx) = mpsc::channel::<()>(8);

        let handle = tokio::spawn(generator(
            tx,
            config(SendMode::Adaptive(signal_rx), Some(100)),
        ));

        assert_eq!(rx.recv().await.unwrap().id, 0);
        drop(signal_tx);

        tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .unwrap()
            .unwrap();
    }
}
