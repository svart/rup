use std::time::Instant;

use tokio::sync::mpsc::{Sender, Receiver};

use crate::pinger::{Entry, Request, Response, StatEntry};

pub(crate) mod async_udp;
pub(crate) mod async_tcp;
pub(crate) mod async_icmp;

pub(crate) trait Transport {
    async fn send(self: &Self, req: &Request) -> Instant;
    async fn recv(self: &Self) -> Response;
}

pub(crate) async fn transmitter(
    transport: impl Transport,
    mut from_generator: Receiver<Request>,
    to_statista: Sender<StatEntry>,
) {
    loop {
        let r = from_generator.recv().await;
        match r {
            Some(req) => {
                let timestamp = transport.send(&req).await;
                let s = StatEntry::Open(Entry{id: req.id, ts: timestamp});
                to_statista.send(s).await.expect("tx: couldn't send open stat entry to statista");
            }
            None => {
                break;
            }
        }
    }
}

pub(crate) async fn receiver(
    transport: impl Transport,
    to_statista: Sender<StatEntry>,
) {
    loop {
        tokio::select! {
            req = transport.recv() => {
                let s = StatEntry::Close(Entry{id: req.id, ts: req.timestamp});
                to_statista.send(s).await.expect("rx: couldn't send close stat entry to statista");
            }
            _ = tokio::signal::ctrl_c() => {
                return;
            }
        }
    }
}
