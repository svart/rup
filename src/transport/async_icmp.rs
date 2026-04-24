use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Instant,
};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

use crate::pinger::{Echo, Request, Response, PING_HDR_LEN};
use crate::transport::Transport;

const IP_HEADER_LEN: usize = 20;
const ICMP_HEADER_LEN: usize = 8;
const DATA_OFFSET: usize = IP_HEADER_LEN + ICMP_HEADER_LEN;

#[derive(Clone)]
pub(crate) struct IcmpClientTransport {
    sock: Arc<UdpSocket>,
    remote_address: SocketAddr,
}

impl IcmpClientTransport {
    pub(crate) async fn new(local: SocketAddr, mut remote: SocketAddr) -> Self {
        let sock = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4))
            .expect("should be able to create socket");
        sock.bind(&local.into())
            .expect("should be able to bind to local address");
        sock.set_nonblocking(true)
            .expect("should be able to set nonblocking for socket");
        let sock = UdpSocket::from_std(sock.into())
            .expect("should be able to create async socket from fd");

        remote.set_port(0);
        sock.connect(remote)
            .await
            .expect("pinger: should be able to connect socket");

        IcmpClientTransport {
            sock: Arc::new(sock),
            remote_address: remote,
        }
    }
}

impl Transport for IcmpClientTransport {
    async fn send(self: &Self, req: &Request) -> Instant {
        let r = Echo {
            id: req.id,
            len: req.request_size.unwrap_or(PING_HDR_LEN as u16),
            resp_size: req.response_size.unwrap_or(PING_HDR_LEN as u16),
        };

        let mut packet = vec![
            0x08, 0x00,
            0x00, 0x00,
            0x12, 0x34,
            (req.id >> 8) as u8, (req.id & 0xff) as u8,
        ];

        let payload = bincode::serialize(&r).unwrap();
        let data_len = req.request_size.unwrap_or(PING_HDR_LEN as u16) as usize;
        packet.extend_from_slice(&payload);
        packet.resize(ICMP_HEADER_LEN + data_len, 0);

        let checksum = csum16_slice(&packet);
        packet[2] = (checksum >> 8) as u8;
        packet[3] = (checksum & 0xff) as u8;

        let timestamp = Instant::now();
        self.sock.send(&packet).await.expect("ICMP tx: failed to send");
        timestamp
    }

    async fn recv(self: &Self) -> Response {
        let mut buf = [0; u16::MAX as usize];

        loop {
            let n = self.sock.recv(&mut buf).await.expect("ICMP rx: failed to recv");

            if n < DATA_OFFSET + PING_HDR_LEN {
                continue;
            }

            if packet_is_good(&buf, &self.remote_address) {
                let echo: Echo = bincode::deserialize(
                    &buf[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN],
                )
                .unwrap();

                return Response {
                    id: echo.id,
                    timestamp: Instant::now(),
                    size: echo.len as usize,
                };
            }
        }
    }
}

fn csum16_add(x: u16, y: u16) -> u16 {
    let s = (x as u32) + (y as u32);

    if s & 0x1_00_00 > 0 {
        (s + 1) as u16
    } else {
        s as u16
    }
}

fn csum16_slice(data: &[u8]) -> u16 {
    assert!(data.len() % 2 == 0);

    let mut csum = 0;
    for chunk in data.chunks_exact(2) {
        let hi = chunk[0] as u16;
        let lo = chunk[1] as u16;
        csum = csum16_add(csum, (hi << 8) | lo);
    }

    !csum
}

fn packet_is_good(buf: &[u8], remote_address: &SocketAddr) -> bool {
    let ip = &buf[..IP_HEADER_LEN];
    let ip_addr = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
    if remote_address.ip() != ip_addr {
        return false;
    }

    let icmp = &buf[IP_HEADER_LEN..IP_HEADER_LEN + ICMP_HEADER_LEN];
    if icmp[0] != 0x00 || icmp[1] != 0x00 {
        return false;
    }
    if icmp[4] != 0x12 || icmp[5] != 0x34 {
        return false;
    }

    true
}
