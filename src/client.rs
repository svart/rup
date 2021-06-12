use std::cmp::Ordering;
use std::collections::VecDeque;
use std::io::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use mio::{Interest, Poll, Waker};
use mio::event::Source;

use crate::common::{TOKEN_SEND_TIMEOUT, TOKEN_READ_SOCKET};

pub(crate) struct TimeStamp {
    pub id: u64,
    pub timestamp: Instant,
}

pub(crate) struct Client<T> {
    pub remote_address: SocketAddr,
    pub send_interval: Option<u64>,
    pub ts_queue: VecDeque<TimeStamp>,
    pub msg_id_counter: u64,
    pub socket: T,
}

impl<T> Client<T> {
    pub fn timestamps_walk(&mut self, msg_id: u64, rtts: &mut Vec<u128>) {
        while let Some(time_stamp) = self.ts_queue.pop_front() {
            match msg_id.cmp(&time_stamp.id) {
                Ordering::Greater => continue,
                Ordering::Equal => rtts.push(time_stamp.timestamp.elapsed().as_micros()),
                Ordering::Less => self.ts_queue.push_front(time_stamp),
            }
            break;
        }
    }
}

pub fn setup_client_polling<T: Source>(socket: &mut T, send_packet_interval: Option<u64>) -> Result<Poll> {
    let poller = Poll::new()?;
    poller.registry()
          .register(socket, TOKEN_READ_SOCKET, Interest::READABLE)?;

    if send_packet_interval.is_some() {
        let waker = Arc::new(Waker::new(poller.registry(), TOKEN_SEND_TIMEOUT)?);
        let _handle = thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(send_packet_interval.unwrap()));
                waker.wake().expect("unable to wake");
            }
        });
    }
    Ok(poller)
}

