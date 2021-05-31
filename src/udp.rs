use std::collections::VecDeque;
use std::io::Result;
use std::net::SocketAddr;
use std::str;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::cmp::{max, Ordering};

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
    let buf: [u8; 8] = current_counter.to_be_bytes();
    let now = Instant::now();
    socket.send_to(&buf, target)?;
    msg_queue.push_back(TimeStamp{id: *current_counter, timestamp: now});
    *current_counter += 1;
    return Ok(now);
}

struct TimeStamp {
    id: u64,
    timestamp: Instant,
}

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

    let mut ts_queue: VecDeque<TimeStamp> = VecDeque::new();
    let mut msg_id_counter: u64 = 0;

    println!("Running UDP client sending pings to {}:{}", address, port);

    let mut socket = UdpSocket::bind("0.0.0.0:0".parse().unwrap())?;
    let mut poll = setup_polling(Poll::new()?, &mut socket, interval)?;
    let mut events = Events::with_capacity(1024);
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
                        process_udp_read(&socket, &mut ts_queue, interval,  &mut now);
                        if interval.is_none() {
                            now = send_buf_to_udp_sock(&socket,
                                                 target_addr.clone(),
                                          &mut msg_id_counter,
                                             &mut ts_queue)?;
                        }
                    }
                }
                TOKEN_TIMEOUT => {
                    now = send_buf_to_udp_sock(&socket,
                                         target_addr.clone(),
                                  &mut msg_id_counter,
                                     &mut ts_queue)?;
                }
                Token(_) => unreachable!(),
            }
        }
    }
}

fn process_udp_read(socket: &UdpSocket,
                    ts_queue: &mut VecDeque<TimeStamp>,
                    interval: Option<u64>,
                    now: &Instant) {
    let mut rcv_buf = [0; UDP_MSG_LEN];
    while let Ok(_) = socket.recv(&mut rcv_buf) {
        let recv_msg_id = u64::from_be_bytes(rcv_buf);
        while let Some(time_stamp) = ts_queue.pop_front() {
            match recv_msg_id.cmp(&time_stamp.id) {
                Ordering::Greater => continue,
                Ordering::Equal => println!("RTT = {} us", time_stamp.timestamp.elapsed().as_micros()),
                Ordering::Less => ts_queue.push_front(time_stamp),
            }
            break;
        }
        if now.elapsed().as_millis() >= interval.unwrap() as u128 {
            break;
        }
    }
}
