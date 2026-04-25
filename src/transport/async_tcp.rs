use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::pinger::{Echo, Request, Response, PING_HDR_LEN};
use crate::transport::Transport;

const IO_TIMEOUT: Duration = Duration::from_secs(30);

async fn server_connection_handler(mut sock: TcpStream) {
    let peer_addr = match sock.peer_addr() {
        Ok(a) => a,
        Err(_) => return,
    };
    println!("New TCP connection from {peer_addr}");

    let mut hdr_buf = [0; PING_HDR_LEN];

    loop {
        match sock.read(&mut hdr_buf).await {
            Ok(0) => {
                println!("Connection closed: {peer_addr}");
                break;
            }
            Ok(amt) => {
                let mut req: Echo = match bincode::deserialize(&hdr_buf) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("Failed to deserialize request from {peer_addr}: {e}");
                        break;
                    }
                };

                if req.len as usize > amt {
                    let remaining = req.len as usize - amt;
                    let mut extra = vec![0; remaining];
                    let mut read = 0;
                    while read < remaining {
                        match sock.read(&mut extra[read..]).await {
                            Ok(0) => {
                                println!("Connection closed: {peer_addr}");
                                return;
                            }
                            Ok(n) => read += n,
                            Err(_) => {
                                eprintln!("Error reading request payload from {peer_addr}");
                                return;
                            }
                        }
                    }
                }

                req.len = req.resp_size;
                req.resp_size = 0;

                let mut send_buf = match bincode::serialize(&req) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("Failed to serialize response: {e}");
                        break;
                    }
                };
                send_buf.resize(req.len as usize, 0);

                if let Err(e) = sock.write_all(&send_buf).await {
                    eprintln!("Error sending echo to {peer_addr}: {e}");
                    break;
                }
            }
            Err(e) => {
                eprintln!("Error reading from {peer_addr}: {e}");
                break;
            }
        }
    }
}

pub(crate) async fn server_transport(local_address: SocketAddr) {
    println!("Running TCP server listening {local_address}");
    let listen_sock = match TcpListener::bind(local_address).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("server: binding failed: {e}");
            return;
        }
    };

    loop {
        tokio::select! {
            result = listen_sock.accept() => {
                match result {
                    Ok((socket, _)) => {
                        tokio::spawn(server_connection_handler(socket));
                    }
                    Err(e) => eprintln!("Connection failed: {e}"),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("TCP server shutting down");
                return;
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct TcpClientTransport {
    stream: Arc<TcpStream>,
}

impl TcpClientTransport {
    pub(crate) fn new(stream: TcpStream) -> Self {
        TcpClientTransport {
            stream: Arc::new(stream),
        }
    }
}

impl Transport for TcpClientTransport {
    async fn send(&self, req: &Request) -> io::Result<Instant> {
        let r = Echo {
            id: req.id,
            len: req.request_size.unwrap_or(PING_HDR_LEN as u16),
            resp_size: req.response_size.unwrap_or(PING_HDR_LEN as u16),
        };

        let mut send_buf = bincode::serialize(&r).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("serialize: {e}"))
        })?;

        if let Some(size) = req.request_size {
            send_buf.resize(size as usize, 0);
        }

        let mut offset = 0;
        while offset < send_buf.len() {
            timeout(IO_TIMEOUT, self.stream.writable())
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "send timeout"))?
                .map_err(|e| io::Error::new(e.kind(), format!("send: writable failed: {e}")))?;

            match self.stream.try_write(&send_buf[offset..]) {
                Ok(n) => offset += n,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(io::Error::new(e.kind(), format!("send failed: {e}"))),
            }
        }

        Ok(Instant::now())
    }

    async fn recv(&self) -> io::Result<Response> {
        let mut hdr = [0; PING_HDR_LEN];
        let mut offset = 0;
        while offset < PING_HDR_LEN {
            timeout(IO_TIMEOUT, self.stream.readable())
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "recv timeout"))?
                .map_err(|e| io::Error::new(e.kind(), format!("recv: readable failed: {e}")))?;

            match self.stream.try_read(&mut hdr[offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "connection closed",
                    ));
                }
                Ok(n) => offset += n,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!("recv header failed: {e}"),
                    ))
                }
            }
        }

        let echo: Echo = bincode::deserialize(&hdr).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("deserialize header: {e}"))
        })?;

        if echo.len as usize > PING_HDR_LEN {
            let remaining = echo.len as usize - PING_HDR_LEN;
            let mut extra = vec![0; remaining];
            let mut offset = 0;
            while offset < remaining {
                timeout(IO_TIMEOUT, self.stream.readable())
                    .await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "recv payload timeout"))?
                    .map_err(|e| {
                        io::Error::new(e.kind(), format!("recv: readable failed: {e}"))
                    })?;

                match self.stream.try_read(&mut extra[offset..]) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionAborted,
                            "connection closed reading payload",
                        ));
                    }
                    Ok(n) => offset += n,
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                    Err(e) => {
                        return Err(io::Error::new(
                            e.kind(),
                            format!("recv payload failed: {e}"),
                        ))
                    }
                }
            }
        }

        Ok(Response {
            id: echo.id,
            timestamp: Instant::now(),
        })
    }
}
