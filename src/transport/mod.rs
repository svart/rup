use std::io;
use std::time::Instant;

use tokio::sync::mpsc::{Receiver, Sender};

use crate::pinger::{Entry, Request, Response, StatEntry};

// Optional transports keep their public API in a dependency-free stub under
// `disabled/`, so callers compile unchanged when the feature is off.
#[cfg_attr(not(feature = "http"), path = "disabled/async_http.rs")]
pub mod async_http;
pub mod async_icmp;
pub mod async_tcp;
pub mod async_udp;

pub trait Transport: Send + Sync {
    fn send(&self, req: &Request) -> impl std::future::Future<Output = io::Result<Instant>> + Send;
    fn recv(&self) -> impl std::future::Future<Output = io::Result<Response>> + Send;
}

pub(crate) async fn transmitter(
    transport: impl Transport,
    mut from_generator: Receiver<Request>,
    to_statista: Sender<StatEntry>,
) -> io::Result<()> {
    while let Some(req) = from_generator.recv().await {
        let timestamp = transport
            .send(&req)
            .await
            .map_err(|error| io::Error::new(error.kind(), format!("send failed: {error}")))?;
        let entry = StatEntry::Open(Entry {
            id: req.id,
            ts: timestamp,
        });
        if to_statista.send(entry).await.is_err() {
            break;
        }
    }
    Ok(())
}

pub(crate) async fn receiver(
    transport: impl Transport,
    to_statista: Sender<StatEntry>,
) -> io::Result<()> {
    loop {
        let response = transport
            .recv()
            .await
            .map_err(|error| io::Error::new(error.kind(), format!("receive failed: {error}")))?;
        if to_statista.send(StatEntry::Close(response)).await.is_err() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PacketSize;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::mpsc;

    /// Records sent requests and answers `recv` from a scripted id queue.
    #[derive(Clone, Default)]
    struct MockTransport {
        sent: Arc<Mutex<Vec<Request>>>,
        replies: Arc<Mutex<VecDeque<u64>>>,
    }

    impl MockTransport {
        fn replying(ids: impl IntoIterator<Item = u64>) -> Self {
            Self {
                replies: Arc::new(Mutex::new(ids.into_iter().collect())),
                ..Self::default()
            }
        }

        fn sent_ids(&self) -> Vec<u64> {
            self.sent.lock().unwrap().iter().map(|req| req.id).collect()
        }
    }

    impl Transport for MockTransport {
        async fn send(&self, req: &Request) -> io::Result<Instant> {
            self.sent.lock().unwrap().push(req.clone());
            Ok(Instant::now())
        }

        async fn recv(&self) -> io::Result<Response> {
            tokio::task::yield_now().await;
            match self.replies.lock().unwrap().pop_front() {
                Some(id) => Ok(Response {
                    id,
                    timestamp: Instant::now(),
                    size: 0,
                    ttl: None,
                }),
                None => Err(io::Error::other("no more responses")),
            }
        }
    }

    #[derive(Clone)]
    struct ErrorTransport;

    impl Transport for ErrorTransport {
        async fn send(&self, _req: &Request) -> io::Result<Instant> {
            Err(io::Error::other("send error"))
        }

        async fn recv(&self) -> io::Result<Response> {
            Err(io::Error::other("recv error"))
        }
    }

    fn request(id: u64) -> Request {
        Request {
            id,
            request_size: None,
            response_size: None,
        }
    }

    async fn queued_requests(requests: impl IntoIterator<Item = Request>) -> Receiver<Request> {
        let (tx, rx) = mpsc::channel(8);
        for req in requests {
            tx.send(req).await.unwrap();
        }
        rx
    }

    #[tokio::test]
    async fn transmitter_sends_each_request_and_opens_it_in_order() {
        let sized = Request {
            request_size: Some(PacketSize::new(64).unwrap()),
            response_size: Some(PacketSize::new(128).unwrap()),
            ..request(1)
        };
        let req_rx = queued_requests([request(0), sized, request(2)]).await;
        let (stat_tx, mut stat_rx) = mpsc::channel(8);
        let transport = MockTransport::default();

        transmitter(transport.clone(), req_rx, stat_tx)
            .await
            .unwrap();

        let mut opened = Vec::new();
        while let Some(entry) = stat_rx.recv().await {
            match entry {
                StatEntry::Open(e) => opened.push(e.id),
                StatEntry::Close(_) => panic!("expected Open entry"),
            }
        }
        assert_eq!(opened, vec![0, 1, 2]);
        assert_eq!(transport.sent_ids(), vec![0, 1, 2]);
        let sent = transport.sent.lock().unwrap();
        assert_eq!(sent[1].request_size, Some(PacketSize::new(64).unwrap()));
        assert_eq!(sent[1].response_size, Some(PacketSize::new(128).unwrap()));
    }

    #[tokio::test(start_paused = true)]
    async fn transmitter_waits_for_statista_capacity_before_next_send() {
        let req_rx = queued_requests((0..3).map(request)).await;
        let (stat_tx, mut stat_rx) = mpsc::channel(1);
        let transport = MockTransport::default();

        let handle = tokio::spawn(transmitter(transport.clone(), req_rx, stat_tx));
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Entry 0 fills the channel; request 1 is sent but its Open entry waits.
        assert_eq!(transport.sent_ids(), vec![0, 1]);
        assert!(!handle.is_finished());

        while stat_rx.recv().await.is_some() {}
        handle.await.unwrap().unwrap();
        assert_eq!(transport.sent_ids(), vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn transmitter_stops_when_statista_is_gone() {
        let req_rx = queued_requests((0..3).map(request)).await;
        let (stat_tx, stat_rx) = mpsc::channel(8);
        drop(stat_rx);
        let transport = MockTransport::default();

        transmitter(transport.clone(), req_rx, stat_tx)
            .await
            .unwrap();

        assert_eq!(transport.sent_ids(), vec![0]);
    }

    #[tokio::test]
    async fn transmitter_returns_send_error() {
        let req_rx = queued_requests([request(0)]).await;
        let (stat_tx, _stat_rx) = mpsc::channel(8);

        let error = transmitter(ErrorTransport, req_rx, stat_tx)
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "send failed: send error");
    }

    #[tokio::test]
    async fn receiver_forwards_close_entries_in_order() {
        let (stat_tx, mut stat_rx) = mpsc::channel(8);

        // The mock errors once its script is exhausted, which ends the receiver.
        receiver(MockTransport::replying(0..3), stat_tx)
            .await
            .unwrap_err();

        let mut closed = Vec::new();
        while let Some(entry) = stat_rx.recv().await {
            match entry {
                StatEntry::Close(response) => closed.push(response.id),
                StatEntry::Open(_) => panic!("expected Close entry"),
            }
        }
        assert_eq!(closed, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn receiver_stops_when_statista_is_gone() {
        // Capacity 1 keeps the receiver from draining the whole script before
        // the drop, so the only way to finish is the closed channel.
        let (stat_tx, mut stat_rx) = mpsc::channel(1);
        let transport = MockTransport::replying(0..3);

        let handle = tokio::spawn(receiver(transport.clone(), stat_tx));
        assert!(stat_rx.recv().await.is_some());
        drop(stat_rx);

        tokio::time::timeout(Duration::from_millis(100), handle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn receiver_returns_recv_error() {
        let (stat_tx, _stat_rx) = mpsc::channel(8);

        let error = receiver(ErrorTransport, stat_tx).await.unwrap_err();

        assert_eq!(error.to_string(), "receive failed: recv error");
    }
}
