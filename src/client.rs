use std::cmp::Ordering;
use std::collections::VecDeque;
use std::io::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use mio::{Interest, Poll, Waker};
use mio::event::Source;
use mio::{Events, Token};
use std::cmp::max;

use crate::common::{DEFAULT_READ_RESPONSE_TIMEOUT, TOKEN_SEND_TIMEOUT, TOKEN_READ_SOCKET};
use crate::pinger::Pinger;

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

impl<T> Client<T> where
    T: Source,
    Client<T>: Pinger {
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

    pub fn ping_loop(&mut self) -> Result<()> {
        let read_response_timeout = match self.send_interval {
            Some(value) => max(DEFAULT_READ_RESPONSE_TIMEOUT, value * 2),
            None => DEFAULT_READ_RESPONSE_TIMEOUT
        };

        let mut poll = setup_client_polling(&mut self.socket, self.send_interval)?;
        let mut events = Events::with_capacity(1024);
        let mut now = self.send_req()?;
        loop {
            poll.poll(&mut events, Some(Duration::from_millis(read_response_timeout)))?;
            // poll timeout, no events
            if events.is_empty() {
                println!("Receive timeout");
                self.ts_queue.pop_front();
                now = self.send_req()?;
                continue;
            }
            for event in events.iter() {
                match event.token() {
                    TOKEN_READ_SOCKET => {
                        if event.is_readable() {
                            for rtt in self.recv_resp(now)? {
                                println!("RTT = {} us", rtt)
                            }
                            if self.send_interval.is_none() {
                                now = self.send_req()?;
                            }
                        }
                    }
                    TOKEN_SEND_TIMEOUT => {
                        now = self.send_req()?;
                    }
                    Token(_) => unreachable!(),
                }
            }
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

