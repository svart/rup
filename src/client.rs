use std::collections::VecDeque;
use std::time::Instant;
use std::net::SocketAddr;
use std::io::{Result, Read, Write};
use std::cmp::Ordering;

use mio::net::{TcpStream, UdpSocket};

const PING_MSG_LEN: usize = 8;

pub struct TimeStamp {
    id: u64,
    timestamp: Instant,
}

pub struct Client<T> {
    remote_address: SocketAddr,
    pub send_interval: Option<u64>,
    pub ts_queue: VecDeque<TimeStamp>,
    msg_id_counter: u64,
    pub socket: T,
}

pub trait Pinger {
    fn send_req(&mut self) -> Result<Instant>;
    fn recv_resp(&mut self, last_send: Instant) -> Result<Vec<u128>>;
}

impl<T> Client<T> {
    fn timestamps_walk(&mut self, msg_id: u64, rtts: &mut Vec<u128>) {
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

impl Client<UdpSocket> {
    pub fn new(address: &str, port: &str, interval: Option<u64>) -> Result<Client<UdpSocket>> {
        let c: Client<UdpSocket> = Client {
            remote_address: format!("{}:{}", address, port).parse().unwrap(),
            send_interval: interval,
            ts_queue: VecDeque::new(),
            msg_id_counter: 0,
            socket: UdpSocket::bind("0.0.0.0:0".parse().unwrap())?
        };
        Ok(c)
    }
}

impl Pinger for Client<UdpSocket> {
    fn send_req(&mut self) -> Result<Instant> {
        let buf: [u8; 8] = self.msg_id_counter.to_be_bytes();
        let now = Instant::now();
        self.socket.send_to(&buf, self.remote_address)?;
        self.ts_queue.push_back(TimeStamp{id: self.msg_id_counter, timestamp: now});
        self.msg_id_counter += 1;
        Ok(now)
    }

    fn recv_resp(&mut self, last_send: Instant) -> Result<Vec<u128>> {
        let mut rtts: Vec<u128> = Vec::new();
        let mut rcv_buf = [0; PING_MSG_LEN];
        while let Ok(_) = self.socket.recv(&mut rcv_buf) {
            let recv_msg_id = u64::from_be_bytes(rcv_buf);
            self.timestamps_walk(recv_msg_id, &mut rtts);
            if last_send.elapsed().as_millis() >= self.send_interval.unwrap_or(0) as u128 {
                break;
            }
        }
        Ok(rtts)
    }
}

impl Client<TcpStream> {
    pub fn new(address: &str, port: &str, interval: Option<u64>) -> Result<Client<TcpStream>> {
        let sock_addr: SocketAddr = format!("{}:{}", address, port).parse().unwrap();
        let c: Client<TcpStream> = Client {
            remote_address: sock_addr,
            send_interval: interval,
            ts_queue: VecDeque::new(),
            msg_id_counter: 0,
            socket: TcpStream::connect(sock_addr)?
        };
        Ok(c)
    }
}

impl Pinger for Client<TcpStream> {
    fn send_req(&mut self) -> Result<Instant> {
        let buf: [u8; 8] = self.msg_id_counter.to_be_bytes();
        let now = Instant::now();
        self.socket.write(&buf)?;
        self.ts_queue.push_back(TimeStamp{id: self.msg_id_counter, timestamp: now});
        self.msg_id_counter += 1;
        Ok(now)
    }

    fn recv_resp(&mut self, last_send: Instant) -> Result<Vec<u128>> {
        let mut rtts: Vec<u128> = Vec::new();
        let mut rcv_buf = [0; PING_MSG_LEN];
        while let Ok(len) = self.socket.read(&mut rcv_buf) {
            if len == 0 {
                break;
            }
            let recv_msg_id = u64::from_be_bytes(rcv_buf);
            self.timestamps_walk(recv_msg_id, &mut rtts);
            if last_send.elapsed().as_millis() >= self.send_interval.unwrap_or(0) as u128 {
                break;
            }
        }
        Ok(rtts)
    }
}
