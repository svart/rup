use std::io::Result;
use std::str;
use std::time::Instant;
use std::collections::VecDeque;

use mio::net::UdpSocket;
use mio::Events;

use crate::client::{Client, TimeStamp};
use crate::pinger::{Pinger, PING_MSG_LEN};
use crate::server::setup_server_polling;



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
        self.msg_id_counter = self.msg_id_counter.wrapping_add(1);
        Ok(now)
    }

    fn recv_resp(&mut self, last_send: Instant) -> Result<Vec<u128>> {
        let mut rtts: Vec<u128> = Vec::new();
        let mut rcv_buf = [0; PING_MSG_LEN];
        while self.socket.recv(&mut rcv_buf).is_ok() {
            let recv_msg_id = u64::from_be_bytes(rcv_buf);
            self.timestamps_walk(recv_msg_id, &mut rtts);
            if last_send.elapsed().as_millis() >= self.send_interval.unwrap_or(0) as u128 {
                break;
            }
        }
        Ok(rtts)
    }

    fn send_err_handler(&self, _: std::io::Error) -> Result<()> {
        // Sending buffer here may be full due to frequent sending
        Ok(())
    }
}

pub fn run_server(local_address: &str, local_port: &str) -> Result<()> {
    println!("Running UDP server listening {}:{}", local_address, local_port);

    let mut socket = UdpSocket::bind(format!("{}:{}", local_address, local_port).parse().unwrap())?;

    let mut poll = setup_server_polling(&mut socket)?;
    let mut events = Events::with_capacity(128);

    loop {
        poll.poll(&mut events, None)?;
        for event in events.iter() {
            if event.is_readable() {
                loop {
                    let mut buf = [0; PING_MSG_LEN];
                    match socket.recv_from(&mut buf) {
                        Ok((amt, src)) => {
                            socket.send_to(&buf[..amt], src)?;
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

pub fn run_client(address: &str, port: &str, interval: Option<u64>) -> Result<()> {
    println!("Running UDP client sending pings to {}:{}", address, port);
    let mut client = <Client<UdpSocket>>::new(address, port, interval)?;
    client.ping_loop()
}