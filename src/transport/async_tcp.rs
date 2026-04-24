use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::pinger::{Echo, Request, Response, PING_HDR_LEN};
use crate::transport::Transport;

async fn server_connection_handler(mut sock: TcpStream) {
    let peer_addr = sock.peer_addr().unwrap();
    println!("New TCP connection from {peer_addr}");

    let mut hdr_buf = [0; PING_HDR_LEN];

    loop {
        match sock.read(&mut hdr_buf).await {
            Ok(0) => {
                println!("Connection closed: {peer_addr}",);
                break;
            }
            Ok(amt) => {
                let mut req: Echo = bincode::deserialize(&hdr_buf).unwrap();

                if req.len as usize > amt {
                    let mut for_read_buf = vec![0; req.len as usize - amt];

                    match sock.read(&mut for_read_buf).await {
                        Ok(0) => {
                            println!("Connection closed: {peer_addr}");
                            break;
                        }
                        Ok(_) => {}
                        Err(_) => {
                            println!(
                                "An error occurred during reading request, \
                                    terminating connection with {peer_addr}"
                            );
                            break;
                        }
                    }
                }
                req.len = req.resp_size;
                req.resp_size = 0;

                let mut send_buf = bincode::serialize(&req).unwrap();
                send_buf.resize(req.len as usize, 0);

                match sock.write_all(&send_buf).await {
                    Err(e) => {
                        println!(
                            "An error occured during writing echo, \
                                    terminating connection with {peer_addr}: {e}"
                        );
                        break;
                    }
                    _ => continue,
                }
            }
            Err(_) => {
                println!(
                    "An error occurred during reading request, \
                          terminating connection with {peer_addr}"
                );
                break;
            }
        }
    }
}

pub(crate) async fn server_transport(local_address: SocketAddr) {
    println!("Running TCP server listening {local_address}");
    let listen_sock = TcpListener::bind(local_address)
        .await
        .expect("server: binding failed");

    loop {
        match listen_sock.accept().await {
            Ok((socket, _)) => {
                tokio::spawn(server_connection_handler(socket));
            }
            Err(e) => println!("Connection failed: {e}"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct TcpClientTransport {
    stream: Arc<TcpStream>,
}

impl TcpClientTransport {
    pub(crate) fn new(stream: TcpStream) -> Self {
        TcpClientTransport { stream: Arc::new(stream) }
    }
}

impl Transport for TcpClientTransport {
    async fn send(self: &Self, req: &Request) -> Instant {
        let r = Echo {
            id: req.id,
            len: req.request_size.unwrap_or(PING_HDR_LEN as u16),
            resp_size: req.response_size.unwrap_or(PING_HDR_LEN as u16),
        };

        let mut send_buf = bincode::serialize(&r).unwrap();

        if let Some(size) = req.request_size {
            send_buf.resize(size as usize, 0);
        }

        let mut offset = 0;
        while offset < send_buf.len() {
            self.stream.writable().await.unwrap();
            match self.stream.try_write(&send_buf[offset..]) {
                Ok(n) => offset += n,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => panic!("TCP tx: failed to send message: {e}"),
            }
        }

        Instant::now()
    }

    async fn recv(self: &Self) -> Response {
        let mut hdr = [0; PING_HDR_LEN];
        let mut offset = 0;
        while offset < PING_HDR_LEN {
            self.stream.readable().await.unwrap();
            match self.stream.try_read(&mut hdr[offset..]) {
                Ok(0) => panic!("TCP rx: connection closed"),
                Ok(n) => offset += n,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => panic!("TCP rx: failed to read header: {e}"),
            }
        }

        let echo: Echo = bincode::deserialize(&hdr).unwrap();

        if echo.len as usize > PING_HDR_LEN {
            let remaining = echo.len as usize - PING_HDR_LEN;
            let mut extra = vec![0; remaining];
            let mut offset = 0;
            while offset < remaining {
                self.stream.readable().await.unwrap();
                match self.stream.try_read(&mut extra[offset..]) {
                    Ok(0) => panic!("TCP rx: connection closed reading payload"),
                    Ok(n) => offset += n,
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(e) => panic!("TCP rx: failed to read payload: {e}"),
                }
            }
        }

        Response {
            id: echo.id,
            timestamp: Instant::now(),
        }
    }
}
