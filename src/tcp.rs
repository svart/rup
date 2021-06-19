use std::io::{Result, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Instant;
use std::net::SocketAddr;
use std::collections::VecDeque;
use mio::net::TcpStream as MioTcpStream;

use crate::client::{Client, TimeStamp};
use crate::pinger::{Pinger, PING_MSG_LEN};

impl Client<MioTcpStream> {
    pub fn new(address: &str, port: &str, interval: Option<u64>) -> Result<Client<MioTcpStream>> {
        let sock_addr: SocketAddr = format!("{}:{}", address, port).parse().unwrap();
        let tcp_sock = TcpStream::connect(sock_addr)?;
        tcp_sock.set_nonblocking(true)?;
        let c: Client<MioTcpStream> = Client {
            remote_address: sock_addr,
            send_interval: interval,
            ts_queue: VecDeque::new(),
            msg_id_counter: 0,
            socket: MioTcpStream::from_std(tcp_sock),
        };
        Ok(c)
    }
}

impl Pinger for Client<MioTcpStream> {
    fn send_req(&mut self) -> Result<Instant> {
        let buf: [u8; 8] = self.msg_id_counter.to_be_bytes();
        let now = Instant::now();
        self.socket.write_all(&buf)?;
        self.ts_queue.push_back(TimeStamp{id: self.msg_id_counter, timestamp: now});
        self.msg_id_counter = self.msg_id_counter.wrapping_add(1);
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

    fn send_err_handler(&self, err: std::io::Error) -> Result<()> {
        Err(err)
    }
}

fn server_handler(mut stream: TcpStream) {
    let peer_addr = stream.peer_addr().unwrap();
    println!("Incoming connection from {}", peer_addr);
    loop {
        let mut read = [0; PING_MSG_LEN];
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

pub fn run_server(local_address: &str, local_port: &str) -> Result<()> {
    println!("Running TCP server listening {}:{}", local_address, local_port);

    let listener = TcpListener::bind(format!("{}:{}", local_address, local_port))?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    server_handler(stream);
                });
            }
            Err(e) => {
                println!("Connection failed: {}", e);
            }
        }
    }
    Ok(())
}

pub fn run_client(address: &str, port: &str, interval: Option<u64>) -> Result<()> {
    println!("Running TCP client connecting to {}:{}", address, port);
    let mut client = <Client<MioTcpStream>>::new(address, port, interval).unwrap();
    client.ping_loop()
}
