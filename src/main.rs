#[macro_use]
extern crate clap;
use clap::App;
use std::net::{TcpListener, TcpStream, UdpSocket, SocketAddr};
use std::io::{Result, Read, Write};
use std::thread;
use std::time::{Duration, Instant};
use std::str;

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

fn run_udp_server(local_address: &str, local_port: &str) -> Result<()> {
    println!("Running UDP server listening {}:{}", local_address, local_port);
    let socket = UdpSocket::bind(format!("{}:{}", local_address, local_port))?;

    let mut source_addr: Option<SocketAddr> = None;
    loop {
        let mut buf = [0; 1024];
        let (amt, src) = socket.recv_from(&mut buf)?;

        if source_addr.is_none() || source_addr.unwrap() != src {
            source_addr = Some(src.clone());
            println!("Receiving pings from {}", src);
        }

        socket.send_to(&buf[..amt], &src)?;
    }
}

fn run_udp_client(address: &str, port: &str) -> Result<()> {
    println!("Running UDP client sending pings to {}:{}", address, port);
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_read_timeout(Some(Duration::new(1, 0)))?;

    loop {
        let snd_buf = b"Ping message";
        let mut rcv_buf = [0; 1024];

        socket.send_to(snd_buf, format!("{}:{}", address, port))?;
        let now = Instant::now();

        match socket.recv(&mut rcv_buf) {
            Ok(_) => {
                println!("RTT = {} us", now.elapsed().as_micros())
            },
            Err(e) => {
                println!("Error reading: {}", e);
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

        match protocol {
            "tcp" => run_tcp_client(remote_address, remote_port),
            "udp" => run_udp_client(remote_address, remote_port),
            _ => unimplemented!(),
        }
    } else if protocol == "icmp" {
        unimplemented!()
    } else {
        panic!("Mode should be set or protocol should be icmp!");
    }
}
