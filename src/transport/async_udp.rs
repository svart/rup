use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Instant;

use tokio::net::UdpSocket;

use crate::pinger::{Echo, Request, Response, PING_HDR_LEN};
use crate::transport::Transport;

pub(crate) async fn server_transport(local_address: SocketAddr) {
    println!("Running UDP server listening {local_address}");

    let sock = UdpSocket::bind(local_address)
        .await
        .expect("server: binding failed");

    let mut client_addrs: HashSet<SocketAddr> = HashSet::new();

    let mut buf = [0; u16::MAX as usize];

    loop {
        let (_, addr) = sock.recv_from(&mut buf).await.unwrap();

        if !client_addrs.contains(&addr) {
            println!("New UDP request from {addr}");
            client_addrs.insert(addr);
        }

        let mut req: Echo = bincode::deserialize(&buf[..PING_HDR_LEN]).unwrap();
        req.len = req.resp_size;
        req.resp_size = 0;

        let mut send_buf = bincode::serialize(&req).unwrap();

        send_buf.resize(req.len as usize, 0);

        sock.send_to(&send_buf, addr).await.unwrap();
    }
}

pub(crate) struct UdpClientTransport {
    socket: UdpSocket,
}

impl UdpClientTransport {
    pub(crate) async fn new(local: SocketAddr, remote: SocketAddr) -> Self {
        let socket = UdpSocket::bind(local)
            .await
            .expect("pinger: binding failed");
        socket.connect(remote)
            .await
            .expect("pinger: connect function failed");

        UdpClientTransport{socket}
    }
}

impl Transport for UdpClientTransport {
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

        let timestamp = Instant::now();
        self.socket.send(&send_buf).await.expect("UDP tx: couldn't send message");
        timestamp
    }

    async fn recv(self: &Self) -> Response {
        let mut buf = [0; u16::MAX as usize];

        self.socket.recv(&mut buf).await.expect("UDP rx: couldn't recv message");

        let r: Echo = bincode::deserialize(&buf[..PING_HDR_LEN]).unwrap();

        Response {
            id: r.id,
            timestamp: Instant::now(),
            size: buf.len(),
        }
    }
}
