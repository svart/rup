use std::collections::VecDeque;
use std::io::Result;
use std::net::SocketAddr;
use std::str;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::cmp::max;

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token, Waker};

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

fn send_buf_to_udp_sock(socket: &UdpSocket,
                        target: SocketAddr,
                        current_counter: &mut u64,
                        msg_queue: &mut VecDeque<TimeStamp>) -> Result<Instant> {
    let now = Instant::now();
    let buf: [u8; 8] = current_counter.to_be_bytes();
    socket.send_to(&buf, target)?;
    msg_queue.push_back(TimeStamp{id: *current_counter, timestamp: now});
    *current_counter += 1;
    return Ok(now);
}

struct TimeStamp {
    id: u64,
    timestamp: Instant,
}

// TODO: Refactor this function. It is too big now.
pub fn run_client(address: &str, port: &str, interval: Option<u64>) -> Result<()> {
    // Times in millis
    let read_response_timeout = match interval {
        Some(value) => max(DEFAULT_READ_RESPONSE_TIMEOUT, value * 2),
        None => DEFAULT_READ_RESPONSE_TIMEOUT
    };

    let mut ts_queue: VecDeque<TimeStamp> = VecDeque::new();
    let mut msg_id_counter: u64 = 0;

    println!("Running UDP client sending pings to {}:{}", address, port);

    let mut socket = UdpSocket::bind("0.0.0.0:0".parse().unwrap())?;

    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(1024);
    poll.registry()
        .register(&mut socket,
                  TOKEN_UDP_SOCKET,
                  Interest::READABLE)?;

    if interval.is_some() {
        let waker = Arc::new(Waker::new(poll.registry(), TOKEN_TIMEOUT)?);
        let _handle = thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(interval.unwrap()));
                waker.wake().expect("unable to wake");
            }
        });
    }

    let target_addr: SocketAddr = format!("{}:{}", address, port).parse().unwrap();
    let mut now = send_buf_to_udp_sock(&socket,
                                       target_addr.clone(),
                                       &mut msg_id_counter,
                                       &mut ts_queue,
    )?;
    loop {
        poll.poll(&mut events, Some(Duration::from_millis(read_response_timeout)))?;

        // poll timeout, no events
        if events.is_empty() {
            println!("Receive timeout");
            ts_queue.pop_front();
            now = send_buf_to_udp_sock(&socket,
                                       target_addr.clone(),
                                       &mut msg_id_counter,
                                       &mut ts_queue,
            )?;
            continue;
        }

        for event in events.iter() {
            match event.token() {
                TOKEN_UDP_SOCKET => {
                    if event.is_readable() {
                        loop {
                            let mut rcv_buf = [0; UDP_MSG_LEN];
                            match socket.recv(&mut rcv_buf) {
                                Ok(_) => {
                                    let recv_msg_id = u64::from_be_bytes(rcv_buf);
                                    loop {
                                        match ts_queue.pop_front() {
                                            Some(time_stamp) => {
                                                if time_stamp.id == recv_msg_id {
                                                    println!("RTT = {} us", time_stamp.timestamp.elapsed().as_micros());
                                                    break;
                                                } else {
                                                    if time_stamp.id > recv_msg_id {
                                                        break;
                                                    }
                                                }
                                            }
                                            None => {
                                                break;
                                            }
                                        }
                                        if now.elapsed().as_millis() >= interval.unwrap() as u128 {
                                            break;
                                        }
                                    }
                                },
                                Err(_) => {
                                    break;
                                }
                            }
                        }
                        if interval.is_none() {
                            match send_buf_to_udp_sock(&socket,
                                                       target_addr.clone(),
                                                       &mut msg_id_counter,
                                                       &mut ts_queue) {
                                Ok(value) => {
                                    now = value;
                                }
                                Err(_) => {
                                    continue;
                                }
                            }
                        }
                    }
                }
                TOKEN_TIMEOUT => {
                    match send_buf_to_udp_sock(&socket,
                                               target_addr.clone(),
                                               &mut msg_id_counter,
                                               &mut ts_queue) {
                        Ok(value) => {
                            now = value;
                        }
                        Err(_) => {
                            continue;
                        }
                    }
                }
                Token(_) => {
                    unreachable!();
                }
            }
        }
    }
}
