use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::net::UdpSocket;

use crate::pinger::{Echo, Request, Response, PING_HDR_LEN};
use crate::transport::Transport;

pub(crate) async fn server_transport(local_address: SocketAddr) {
    println!("Running UDP server listening {local_address}");

    let sock = match UdpSocket::bind(local_address).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("server: binding failed: {e}");
            return;
        }
    };

    let mut client_addrs: HashSet<SocketAddr> = HashSet::new();
    let mut buf = vec![0; u16::MAX as usize];

    loop {
        let (n, addr) = match sock.recv_from(&mut buf).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("server: recv error: {e}");
                continue;
            }
        };

        if !client_addrs.contains(&addr) {
            println!("New UDP request from {addr}");
            client_addrs.insert(addr);
        }

        if n < PING_HDR_LEN {
            eprintln!("server: packet too short from {addr}: {n} bytes");
            continue;
        }

        let mut req: Echo = match bincode::deserialize(&buf[..PING_HDR_LEN]) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("server: failed to deserialize from {addr}: {e}");
                continue;
            }
        };

        req.len = req.resp_size;
        req.resp_size = 0;

        let mut send_buf = match bincode::serialize(&req) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("server: failed to serialize response: {e}");
                continue;
            }
        };

        send_buf.resize(req.len as usize, 0);

        if let Err(e) = sock.send_to(&send_buf, addr).await {
            eprintln!("server: send error to {addr}: {e}");
        }
    }
}

#[derive(Clone)]
pub(crate) struct UdpClientTransport {
    socket: Arc<UdpSocket>,
}

impl UdpClientTransport {
    pub(crate) async fn new(local: SocketAddr, remote: SocketAddr) -> io::Result<Self> {
        let socket = UdpSocket::bind(local).await.map_err(|e| {
            io::Error::new(e.kind(), format!("client bind to {local} failed: {e}"))
        })?;
        socket.connect(remote).await.map_err(|e| {
            io::Error::new(e.kind(), format!("client connect to {remote} failed: {e}"))
        })?;

        Ok(UdpClientTransport {
            socket: Arc::new(socket),
        })
    }
}

impl Transport for UdpClientTransport {
    async fn send(self: &Self, req: &Request) -> io::Result<Instant> {
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

        let timestamp = Instant::now();
        self.socket.send(&send_buf).await?;
        Ok(timestamp)
    }

    async fn recv(self: &Self) -> io::Result<Response> {
        let mut buf = vec![0; u16::MAX as usize];
        let n = self.socket.recv(&mut buf).await?;

        if n < PING_HDR_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("packet too short: {n} bytes"),
            ));
        }

        let r: Echo = bincode::deserialize(&buf[..PING_HDR_LEN]).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("deserialize: {e}"))
        })?;

        Ok(Response {
            id: r.id,
            timestamp: Instant::now(),
        })
    }
}
