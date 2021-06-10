use std::collections::VecDeque;
use std::time::Instant;
use std::net::SocketAddr;
use std::cmp::Ordering;

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


