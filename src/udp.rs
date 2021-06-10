use std::io::Result;
use std::str;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::collections::VecDeque;
use std::cmp::max;

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token, Waker};

use crate::client::{Client, TimeStamp};
use crate::pinger::{Pinger, PING_MSG_LEN};

const TOKEN_UDP_SOCKET: Token = Token(0);
const TOKEN_TIMEOUT: Token = Token(1);

const DEFAULT_READ_RESPONSE_TIMEOUT: u64 = 1000;


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
        self.ts_queue.push_back(TimeStamp { id: self.msg_id_counter, timestamp: now });
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

pub fn run_server(local_address: &str, local_port: &str) -> Result<()> {
    println!("Running UDP server listening {}:{}", local_address, local_port);

    let mut socket = UdpSocket::bind(format!("{}:{}", local_address, local_port).parse().unwrap())?;

    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(128);
    poll.registry()
        .register(&mut socket, TOKEN_UDP_SOCKET, Interest::READABLE)?;

    loop {
        poll.poll(&mut events, None)?;
        for event in events.iter() {
            if event.is_readable() {
                loop {
                    let mut buf = [0; PING_MSG_LEN];
                    match socket.recv_from(&mut buf) {
                        Ok((amt, src)) => {
                            socket.send_to(&buf[..amt], src).unwrap();
                        }
                        Err(_) => {
                            break;
                        }
                    }
                }
            }
        }
    }
}

// TODO: Set tos on the messages
// TODO: Set length of the messages

fn setup_polling(poller: Poll, socket: &mut UdpSocket, send_packet_interval: Option<u64>) -> Result<Poll> {
    poller.registry()
          .register(socket,
                    TOKEN_UDP_SOCKET,
                    Interest::READABLE)?;

    if send_packet_interval.is_some() {
        let waker = Arc::new(Waker::new(poller.registry(), TOKEN_TIMEOUT)?);
        let _handle = thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(send_packet_interval.unwrap()));
                waker.wake().expect("unable to wake");
            }
        });
    }
    Ok(poller)
}

pub fn run_client(address: &str, port: &str, interval: Option<u64>) -> Result<()> {
    // Time in millis
    let read_response_timeout = match interval {
        Some(value) => max(DEFAULT_READ_RESPONSE_TIMEOUT, value * 2),
        None => DEFAULT_READ_RESPONSE_TIMEOUT
    };
    println!("Running UDP client sending pings to {}:{}", address, port);

    let mut client = <Client<UdpSocket>>::new(address, port, interval).unwrap();
    let mut poll = setup_polling(Poll::new()?,
                                      &mut client.socket,
                                      client.send_interval)?;
    let mut events = Events::with_capacity(1024);
    let mut now = client.send_req()?;
    loop {
        poll.poll(&mut events, Some(Duration::from_millis(read_response_timeout)))?;
        // poll timeout, no events
        if events.is_empty() {
            println!("Receive timeout");
            client.ts_queue.pop_front();
            now = client.send_req()?;
            continue;
        }
        for event in events.iter() {
            match event.token() {
                TOKEN_UDP_SOCKET => {
                    if event.is_readable() {
                        for rtt in client.recv_resp(now)? {
                            println!("RTT = {} us", rtt)
                        }
                        if interval.is_none() {
                            now = client.send_req()?;
                        }
                    }
                }
                TOKEN_TIMEOUT => {
                    now = client.send_req()?;
                }
                Token(_) => unreachable!(),
            }
        }
    }
}