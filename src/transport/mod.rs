use std::io;
use std::time::Instant;

use tokio::sync::mpsc::{Receiver, Sender};

use crate::pinger::{Entry, Request, Response, StatEntry};

pub mod async_icmp;
pub mod async_tcp;
pub mod async_udp;

pub trait Transport: Send + Sync {
    fn send(&self, req: &Request) -> impl std::future::Future<Output = io::Result<Instant>> + Send;
    fn recv(&self) -> impl std::future::Future<Output = io::Result<Response>> + Send;
}

pub async fn transmitter(
    transport: impl Transport,
    mut from_generator: Receiver<Request>,
    to_statista: Sender<StatEntry>,
) {
    loop {
        let r = from_generator.recv().await;
        match r {
            Some(req) => match transport.send(&req).await {
                Ok(timestamp) => {
                    let s = StatEntry::Open(Entry {
                        id: req.id,
                        ts: timestamp,
                    });
                    if to_statista.send(s).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("transport error: send failed: {e}");
                    break;
                }
            },
            None => break,
        }
    }
}

pub async fn receiver(transport: impl Transport, to_statista: Sender<StatEntry>) {
    loop {
        tokio::select! {
            result = transport.recv() => {
                match result {
                    Ok(req) => {
                        let s = StatEntry::Close(Entry{id: req.id, ts: req.timestamp});
                        if to_statista.send(s).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("transport error: recv failed: {e}");
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    struct MockTransport {
        send_delay: std::time::Duration,
        recv_responses: Vec<Response>,
        recv_index: std::sync::Arc<std::sync::Mutex<usize>>,
    }

    impl MockTransport {
        fn new(recv_responses: Vec<Response>) -> Self {
            MockTransport {
                send_delay: std::time::Duration::from_micros(1),
                recv_responses,
                recv_index: std::sync::Arc::new(std::sync::Mutex::new(0)),
            }
        }
    }

    impl Clone for MockTransport {
        fn clone(&self) -> Self {
            MockTransport {
                send_delay: self.send_delay,
                recv_responses: self.recv_responses.clone(),
                recv_index: self.recv_index.clone(),
            }
        }
    }

    impl Transport for MockTransport {
        async fn send(&self, _req: &Request) -> io::Result<Instant> {
            tokio::time::sleep(self.send_delay).await;
            Ok(Instant::now())
        }

        async fn recv(&self) -> io::Result<Response> {
            let id = {
                let mut idx = self.recv_index.lock().unwrap();
                if *idx < self.recv_responses.len() {
                    let id = self.recv_responses[*idx].id;
                    *idx += 1;
                    Some(id)
                } else {
                    None
                }
            };

            tokio::time::sleep(std::time::Duration::from_micros(1)).await;
            match id {
                Some(id) => Ok(Response {
                    id,
                    timestamp: Instant::now(),
                }),
                None => Err(io::Error::other("no more responses")),
            }
        }
    }

    #[tokio::test]
    async fn transmitter_sends_open_entry() {
        let (req_tx, req_rx) = mpsc::channel(8);
        let (stat_tx, mut stat_rx) = mpsc::channel(8);
        let transport = MockTransport::new(vec![]);

        req_tx
            .send(Request {
                id: 5,
                request_size: None,
                response_size: None,
            })
            .await
            .unwrap();
        drop(req_tx);

        transmitter(transport, req_rx, stat_tx).await;

        let entry = stat_rx.recv().await.unwrap();
        match entry {
            StatEntry::Open(e) => assert_eq!(e.id, 5),
            _ => panic!("expected Open entry"),
        }
    }

    #[tokio::test]
    async fn receiver_sends_close_entry() {
        let (stat_tx, mut stat_rx) = mpsc::channel(8);
        let transport = MockTransport::new(vec![Response {
            id: 3,
            timestamp: Instant::now(),
        }]);

        let handle = tokio::spawn(async move {
            receiver(transport, stat_tx).await;
        });

        let entry = tokio::time::timeout(std::time::Duration::from_millis(100), stat_rx.recv())
            .await
            .unwrap()
            .unwrap();

        match entry {
            StatEntry::Close(e) => assert_eq!(e.id, 3),
            _ => panic!("expected Close entry"),
        }

        handle.abort();
    }

    #[tokio::test]
    async fn transmitter_exits_on_channel_close() {
        let (req_tx, req_rx) = mpsc::channel::<Request>(8);
        let (stat_tx, _stat_rx) = mpsc::channel(8);
        let transport = MockTransport::new(vec![]);

        drop(req_tx);

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            transmitter(transport, req_rx, stat_tx),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn transmitter_multiple_requests_in_order() {
        let (req_tx, req_rx) = mpsc::channel(8);
        let (stat_tx, mut stat_rx) = mpsc::channel(8);
        let transport = MockTransport::new(vec![]);

        req_tx
            .send(Request {
                id: 0,
                request_size: None,
                response_size: None,
            })
            .await
            .unwrap();
        req_tx
            .send(Request {
                id: 1,
                request_size: None,
                response_size: None,
            })
            .await
            .unwrap();
        req_tx
            .send(Request {
                id: 2,
                request_size: None,
                response_size: None,
            })
            .await
            .unwrap();
        drop(req_tx);

        transmitter(transport, req_rx, stat_tx).await;

        let mut ids = Vec::new();
        while let Some(entry) = stat_rx.recv().await {
            if let StatEntry::Open(e) = entry {
                ids.push(e.id);
            }
        }
        assert_eq!(ids, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn transmitter_passes_request_sizes() {
        let (req_tx, req_rx) = mpsc::channel(8);
        let (stat_tx, mut stat_rx) = mpsc::channel(8);
        let transport = MockTransport::new(vec![]);

        req_tx
            .send(Request {
                id: 10,
                request_size: Some(64),
                response_size: Some(128),
            })
            .await
            .unwrap();
        drop(req_tx);

        transmitter(transport, req_rx, stat_tx).await;

        let entry = stat_rx.recv().await.unwrap();
        match entry {
            StatEntry::Open(e) => assert_eq!(e.id, 10),
            _ => panic!("expected Open"),
        }
    }

    struct ErrorTransport;

    impl Clone for ErrorTransport {
        fn clone(&self) -> Self {
            ErrorTransport
        }
    }

    impl Transport for ErrorTransport {
        async fn send(&self, _req: &Request) -> io::Result<Instant> {
            Err(io::Error::other("send error"))
        }

        async fn recv(&self) -> io::Result<Response> {
            Err(io::Error::other("recv error"))
        }
    }

    #[tokio::test]
    async fn transmitter_send_error_breaks_loop() {
        let (req_tx, req_rx) = mpsc::channel(8);
        let (stat_tx, _stat_rx) = mpsc::channel(8);
        let transport = ErrorTransport;

        req_tx
            .send(Request {
                id: 0,
                request_size: None,
                response_size: None,
            })
            .await
            .unwrap();

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            transmitter(transport, req_rx, stat_tx),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn receiver_exits_on_stat_channel_close() {
        let (stat_tx, mut stat_rx) = mpsc::channel(8);
        let transport = MockTransport::new(vec![Response {
            id: 0,
            timestamp: Instant::now(),
        }]);

        let handle = tokio::spawn(receiver(transport, stat_tx));

        let _ = stat_rx.recv().await;
        drop(stat_rx);

        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), handle)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn receiver_recv_error_breaks_loop() {
        let (stat_tx, _stat_rx) = mpsc::channel(8);
        let transport = ErrorTransport;

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            receiver(transport, stat_tx),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn receiver_multiple_responses_in_order() {
        let (stat_tx, mut stat_rx) = mpsc::channel(8);
        let transport = MockTransport::new(vec![
            Response {
                id: 0,
                timestamp: Instant::now(),
            },
            Response {
                id: 1,
                timestamp: Instant::now(),
            },
            Response {
                id: 2,
                timestamp: Instant::now(),
            },
        ]);

        let handle = tokio::spawn(receiver(transport, stat_tx));

        let mut ids = Vec::new();
        for _ in 0..3 {
            match tokio::time::timeout(std::time::Duration::from_millis(100), stat_rx.recv()).await
            {
                Ok(Some(StatEntry::Close(e))) => ids.push(e.id),
                _ => break,
            }
        }
        assert_eq!(ids, vec![0, 1, 2]);
        handle.abort();
    }

    #[tokio::test]
    async fn transmitter_stat_channel_full_backpressure() {
        let (req_tx, req_rx) = mpsc::channel(1);
        let (stat_tx, _stat_rx) = mpsc::channel(1);
        let transport = MockTransport::new(vec![]);

        req_tx
            .send(Request {
                id: 0,
                request_size: None,
                response_size: None,
            })
            .await
            .unwrap();
        drop(req_tx);

        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            transmitter(transport, req_rx, stat_tx),
        )
        .await
        .unwrap();
    }
}
