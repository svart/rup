use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rup::{
    PacketSize, PingEvent, PingReport, Pinger, Request, Response, SessionConfig, Target,
    TrafficClass, Transport, run_with_transport, start_with_transport,
};
use tokio::sync::{Mutex, mpsc};

/// A downstream transport: replies to every request except the ids in `drop`.
#[derive(Clone)]
struct LoopbackTransport {
    drop: &'static [u64],
    replies_tx: mpsc::UnboundedSender<u64>,
    replies_rx: Arc<Mutex<mpsc::UnboundedReceiver<u64>>>,
}

impl LoopbackTransport {
    fn dropping(drop: &'static [u64]) -> Self {
        let (replies_tx, replies_rx) = mpsc::unbounded_channel();
        Self {
            drop,
            replies_tx,
            replies_rx: Arc::new(Mutex::new(replies_rx)),
        }
    }
}

impl Transport for LoopbackTransport {
    async fn send(&self, req: &Request) -> io::Result<Instant> {
        let sent = Instant::now();
        if !self.drop.contains(&req.id) {
            let _ = self.replies_tx.send(req.id);
        }
        Ok(sent)
    }

    async fn recv(&self) -> io::Result<Response> {
        let id = self.replies_rx.lock().await.recv().await;
        Ok(Response {
            id: id.expect("sender lives in the transport"),
            timestamp: Instant::now(),
            size: 12,
            ttl: None,
        })
    }
}

fn custom_session() -> SessionConfig {
    SessionConfig::default()
        .interval(Duration::from_millis(1))
        .timeout(Duration::from_millis(50))
        .count(4)
}

#[test]
fn high_level_builders_compose_without_exposing_internal_state() {
    let remote = "127.0.0.1:5000".parse().unwrap();
    let packet_size = PacketSize::new(64).unwrap();

    let session = SessionConfig::default()
        .count(5)
        .interval(Duration::from_millis(250))
        .timeout(Duration::from_secs(2))
        .request_size(packet_size)
        .response_size(packet_size);

    let _pinger = Pinger::new(Target::Udp(remote))
        .session(session)
        .traffic_class(TrafficClass::new(184));
}

#[test]
fn report_accessors_preserve_received_sample_consistency() {
    let report =
        PingReport::from_rtts(3, vec![Duration::from_millis(2), Duration::from_millis(4)]).unwrap();

    assert_eq!(report.sent(), 3);
    assert_eq!(report.received(), 2);
    assert_eq!(report.rtts().len(), report.received() as usize);
    assert_eq!(report.mean(), Some(Duration::from_millis(3)));
}

#[test]
fn report_rejects_more_samples_than_sent_packets() {
    let error = PingReport::from_rtts(0, vec![Duration::from_millis(1)]).unwrap_err();

    assert_eq!(error.to_string(), "received count 1 exceeds sent count 0");
}

#[tokio::test]
async fn custom_transport_runs_a_session_to_a_report() {
    let report = run_with_transport(LoopbackTransport::dropping(&[2]), custom_session())
        .await
        .unwrap();

    assert_eq!(report.sent(), 4);
    assert_eq!(report.received(), 3);
    assert_eq!(report.loss_pct(), 25.0);
}

#[tokio::test]
async fn custom_transport_streams_live_events() {
    let mut session = start_with_transport(LoopbackTransport::dropping(&[3]), custom_session());

    let mut replies = Vec::new();
    let mut timeouts = Vec::new();
    while let Some(event) = session.next().await {
        match event {
            PingEvent::Reply(result) => replies.push(result.seq),
            PingEvent::Timeout { seq } => timeouts.push(seq),
            other => panic!("unexpected {other:?}"),
        }
    }

    assert_eq!(replies, vec![0, 1, 2]);
    assert_eq!(timeouts, vec![3]);
    assert_eq!(session.report().await.unwrap().received(), 3);
}
