use std::io::Result;
use std::str;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::cmp::{max};

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token, Waker};

use crate::client::{Client, Pinger};

const TOKEN_UDP_SOCKET: Token = Token(0);
const TOKEN_TIMEOUT: Token = Token(1);

const DEFAULT_READ_RESPONSE_TIMEOUT: u64 = 1000;
const UDP_MSG_LEN: usize = 8;

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
                    let mut buf = [0; UDP_MSG_LEN];
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
    // Times in millis
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