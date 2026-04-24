use std::io;
use std::time::Instant;

use tokio::sync::mpsc::{Sender, Receiver};

use crate::pinger::{Entry, Request, Response, StatEntry};

pub(crate) mod async_udp;
pub(crate) mod async_tcp;
pub(crate) mod async_icmp;

pub(crate) trait Transport {
    async fn send(self: &Self, req: &Request) -> io::Result<Instant>;
    async fn recv(self: &Self) -> io::Result<Response>;
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
