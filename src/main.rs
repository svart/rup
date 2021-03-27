#[macro_use]
extern crate clap;
use std::net::{TcpListener, TcpStream, SocketAddr};
use std::io::{Result, Read, Write};
use std::thread;
use std::time::{Duration, Instant};
use std::str;
use std::sync::Arc;
use std::cmp::max;
use std::collections::VecDeque;

use clap::App;
use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token, Waker};

// TODO: move TCP on MIO
fn tcp_server_handler(mut stream: TcpStream) {
    let peer_addr = stream.peer_addr().unwrap();
    println!("Incoming connection from {}", peer_addr);
    loop {
        let mut read = [0; 1024];
        match stream.read(&mut read) {
            Ok(0) => {
                println!("Connection closed: {}", peer_addr);
                break;
            },
            Ok(n) => {
                match stream.write(&read[0..n]) {
                    Err(e) => {
                        println!("An error occurred during writing echo, \
                                  terminating connection with {}: {}",
                                  peer_addr, e);
                    },
                    _ => continue,
                }
            },
            Err(_) => {
                println!("An error occurred during reading request, \
                          terminating connection with {}", peer_addr);
                break;
            }
        }
    }
}

fn run_tcp_server(local_address: &str, local_port: &str) -> Result<()> {
    println!("Running TCP server listening {}:{}", local_address, local_port);
    let listener = TcpListener::bind(format!("{}:{}", local_address, local_port))?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    tcp_server_handler(stream);
                });
            }
            Err(e) => {
                println!("Connection failed: {}", e);
            }
        }
    }
    Ok(())
}

fn run_tcp_client(address: &str, port: &str) -> Result<()> {
    println!("Running TCP client connecting {}:{}", address, port);
    match TcpStream::connect(format!("{}:{}", address, port)) {
        Ok(mut stream) => {
            loop {
                let msg = b"Ping message";
                stream.write(msg).unwrap();
                let now = Instant::now();

                let mut read = [0; 1024];
                match stream.read(&mut read) {
                    Ok(0) => {
                        println!("Connection closed: {}", stream.peer_addr().unwrap());
                        break;
                    },
                    Ok(_) => {
                        println!("RTT = {} us", now.elapsed().as_micros())
                    },
                    Err(_) => {
                        println!("An error occurred, terminating connection with {}",
                                 stream.peer_addr().unwrap());
                        break;
                    }
                }
            }
        }
        Err(e) => {
            println!("Failed to connect: {}", e);
        }
    }
    Ok(())
}

const UDP_SOCKET: Token = Token(0);
const TIMEOUT: Token = Token(1);

fn run_udp_server(local_address: &str, local_port: &str) -> Result<()> {
    println!("Running UDP server listening {}:{}", local_address, local_port);

    let mut socket = UdpSocket::bind(format!("{}:{}", local_address, local_port).parse().unwrap())?;

    let mut poll = Poll::new()?;
    let mut events = Events::with_capacity(128);
    poll.registry()
        .register(&mut socket, UDP_SOCKET, Interest::READABLE)?;

    loop {
        poll.poll(&mut events, None)?;
        for event in events.iter() {
            if event.is_readable() {
                loop {
                    let mut buf = [0; 8];
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
fn run_udp_client(address: &str, port: &str, interval: Option<u64>) -> Result<()> {
    // Times in millis
    const DEFAULT_READ_RESPONSE_TIMEOUT: u64 = 1000;
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
                  UDP_SOCKET,
                  Interest::READABLE)?;

    if interval.is_some() {
        let waker = Arc::new(Waker::new(poll.registry(), TIMEOUT)?);
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
                UDP_SOCKET => {
                    if event.is_readable() {
                        loop {
                            let mut rcv_buf = [0; 8];
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
                TIMEOUT => {
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

fn main() -> Result<()> {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml).get_matches();

    let protocol = matches.value_of("protocol").unwrap_or("icmp");

    let server_mode = matches.is_present("server");

    let client_mode = matches.is_present("client");

    if server_mode {
        let local_address = matches.value_of("local-address").unwrap_or("0.0.0.0");
        let local_port = matches.value_of("local-port").unwrap();

        match protocol {
            "tcp" => run_tcp_server(local_address, local_port),
            "udp" => run_udp_server(local_address, local_port),
            _ => unimplemented!(),
        }
    } else if client_mode {
        let remote_port = matches.value_of("remote-port").unwrap();
        let remote_address = matches.value_of("remote-address").unwrap();
        let interval: Option<u64> = match matches.value_of("interval") {
            Some(value) => Some(value.parse().unwrap()),
            None => None,
        };

        match protocol {
            "tcp" => run_tcp_client(remote_address, remote_port),
            "udp" => run_udp_client(remote_address, remote_port, interval),
            _ => unimplemented!(),
        }
    } else if protocol == "icmp" {
        unimplemented!()
    } else {
        panic!("Mode should be set or protocol should be icmp!");
    }
}
