use std::io;
use std::time::Instant;

use tokio::sync::mpsc::{Sender, Receiver};

use crate::pinger::{Entry, Request, Response, StatEntry};

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
        fn send(
            &self,
            _req: &Request,
        ) -> impl std::future::Future<Output = io::Result<Instant>> + Send {
            async {
                tokio::time::sleep(self.send_delay).await;
                Ok(Instant::now())
            }
        }

        fn recv(&self) -> impl std::future::Future<Output = io::Result<Response>> + Send {
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
            async move {
                tokio::time::sleep(std::time::Duration::from_micros(1)).await;
                match id {
                    Some(id) => Ok(Response { id, timestamp: Instant::now() }),
                    None => Err(io::Error::new(io::ErrorKind::Other, "no more responses")),
                }
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
}

pub(crate) mod async_udp;
pub(crate) mod async_tcp;
pub(crate) mod async_icmp;

pub(crate) trait Transport: Send + Sync {
    fn send(
        &self,
        req: &Request,
    ) -> impl std::future::Future<Output = io::Result<Instant>> + Send;
    fn recv(&self) -> impl std::future::Future<Output = io::Result<Response>> + Send;
}

pub(crate) async fn transmitter(
    transport: impl Transport,
    mut from_generator: Receiver<Request>,
    to_statista: Sender<StatEntry>,
) {
    loop {
        let r = from_generator.recv().await;
        match r {
            Some(req) => match transport.send(&req).await {
                Ok(timestamp) => {
                    let s = StatEntry::Open(Entry { id: req.id, ts: timestamp });
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

pub(crate) async fn receiver(
    transport: impl Transport,
    to_statista: Sender<StatEntry>,
) {
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
