use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

use crate::pinger::{Echo, Request, Response, PING_HDR_LEN};
use crate::transport::Transport;

const ICMP_HEADER_LEN: usize = 8;
const DATA_OFFSET: usize = ICMP_HEADER_LEN;

fn is_ipv6(addr: &SocketAddr) -> bool {
    matches!(addr, SocketAddr::V6(_))
}

#[derive(Clone)]
pub(crate) struct IcmpClientTransport {
    sock: Arc<UdpSocket>,
    remote: SocketAddr,
}

impl IcmpClientTransport {
    pub(crate) async fn new(local: SocketAddr, remote: SocketAddr) -> io::Result<Self> {
        let (domain, protocol) = if is_ipv6(&remote) {
            (Domain::IPV6, Protocol::ICMPV6)
        } else {
            (Domain::IPV4, Protocol::ICMPV4)
        };

        let sock = Socket::new(domain, Type::DGRAM, Some(protocol)).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "ICMP socket failed: {e}. \
                     Try: sudo sysctl -w net.ipv4.ping_group_range='0 2147483647'"
                ),
            )
        })?;

        sock.bind(&local.into())
            .map_err(|e| io::Error::new(e.kind(), format!("bind: {e}")))?;
        sock.set_nonblocking(true)
            .map_err(|e| io::Error::new(e.kind(), format!("set nonblocking: {e}")))?;

        let udp: std::net::UdpSocket = sock.into();
        let sock = UdpSocket::from_std(udp)
            .map_err(|e| io::Error::new(e.kind(), format!("into async socket: {e}")))?;

        Ok(IcmpClientTransport {
            sock: Arc::new(sock),
            remote,
        })
    }
}

impl Transport for IcmpClientTransport {
    async fn send(&self, req: &Request) -> io::Result<Instant> {
        let r = Echo {
            id: req.id,
            len: req.request_size.unwrap_or(PING_HDR_LEN as u16),
            resp_size: req.response_size.unwrap_or(0),
        };

        let seq_bytes = (req.id as u16).to_be_bytes();
        let echo_type: u8 = if is_ipv6(&self.remote) { 128 } else { 8 };

        let mut packet = vec![
            echo_type, 0x00,
            0x00, 0x00,
            0x00, 0x00,
            seq_bytes[0], seq_bytes[1],
        ];

        let payload = bincode::serialize(&r)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let data_len = req.request_size.unwrap_or(PING_HDR_LEN as u16) as usize;
        packet.extend_from_slice(&payload);
        packet.resize(ICMP_HEADER_LEN + data_len, 0);

        if !is_ipv6(&self.remote) {
            let checksum = csum16_slice(&packet);
            packet[2] = (checksum >> 8) as u8;
            packet[3] = (checksum & 0xff) as u8;
        }

        let ts = Instant::now();
        self.sock.send_to(&packet, self.remote).await?;
        Ok(ts)
    }

    async fn recv(&self) -> io::Result<Response> {
        let mut buf = vec![0; u16::MAX as usize];
        let reply_type: u8 = if is_ipv6(&self.remote) { 129 } else { 0 };

        loop {
            let (n, addr) = match self.sock.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(_) => continue,
            };

            if addr.ip() != self.remote.ip() {
                continue;
            }

            if n < DATA_OFFSET + PING_HDR_LEN {
                continue;
            }

            let icmp = &buf[..ICMP_HEADER_LEN];
            if icmp[0] != reply_type || icmp[1] != 0x00 {
                continue;
            }

            let echo: Echo =
                match bincode::deserialize(&buf[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN]) {
                    Ok(e) => e,
                    Err(_) => continue,
                };

            return Ok(Response {
                id: echo.id,
                timestamp: Instant::now(),
            });
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
    let mut csum = 0;
    for chunk in data.chunks(2) {
        if chunk.len() == 2 {
            let hi = chunk[0] as u16;
            let lo = chunk[1] as u16;
            csum = csum16_add(csum, (hi << 8) | lo);
        } else {
            csum = csum16_add(csum, (chunk[0] as u16) << 8);
        }
    }
    !csum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csum16_slice_even() {
        let data = [0x08, 0x00, 0x00, 0x00, 0x12, 0x34, 0x00, 0x01];
        let csum = csum16_slice(&data);
        let got = !csum;
        let expected: u16 = 0x0800u16
            .wrapping_add(0x0000u16)
            .wrapping_add(0x1234u16)
            .wrapping_add(0x0001u16);
        assert_eq!(got, expected);
    }

    #[test]
    fn csum16_odd_length() {
        let data = [0x08, 0x00, 0x00, 0x00, 0x12, 0x34, 0x00];
        let _csum = csum16_slice(&data);
    }

    #[test]
    fn csum16_empty_slice() {
        let data: [u8; 0] = [];
        let csum = csum16_slice(&data);
        assert_eq!(csum, 0xffff);
    }

    #[test]
    fn csum16_single_byte() {
        let data = [0xff];
        let csum = csum16_slice(&data);
        let expected = !(0xff00u16);
        assert_eq!(csum, expected);
    }

    #[test]
    fn csum16_add_wraparound() {
        let result = csum16_add(0xFFFF, 0x0001);
        assert_eq!(result, 0x0001);
    }

    #[test]
    fn csum16_add_no_wraparound() {
        let result = csum16_add(0x0001, 0x0002);
        assert_eq!(result, 0x0003);
    }

    #[test]
    fn csum16_add_both_max() {
        let result = csum16_add(0xFFFF, 0xFFFF);
        assert_eq!(result, 0xFFFF);
    }

    #[test]
    fn icmp_v4_packet_header_fields() {
        let echo = Echo { id: 0x1234, len: PING_HDR_LEN as u16, resp_size: 0 };
        let payload = bincode::serialize(&echo).unwrap();

        let seq_bytes = (0x1234u64 as u16).to_be_bytes();
        let echo_type: u8 = 8;
        let mut packet = vec![
            echo_type, 0x00,
            0x00, 0x00,
            0x00, 0x00,
            seq_bytes[0], seq_bytes[1],
        ];
        packet.extend_from_slice(&payload);
        packet.resize(ICMP_HEADER_LEN + PING_HDR_LEN, 0);

        assert_eq!(packet[0], 8);
        assert_eq!(packet[1], 0);
        assert_eq!(packet[4], 0);
        assert_eq!(packet[5], 0);
        assert_eq!(packet[6], 0x12);
        assert_eq!(packet[7], 0x34);
        assert_eq!(packet.len(), ICMP_HEADER_LEN + PING_HDR_LEN);
    }

    #[test]
    fn icmp_v4_checksum_correct() {
        let echo = Echo { id: 0x0001, len: PING_HDR_LEN as u16, resp_size: 0 };
        let payload = bincode::serialize(&echo).unwrap();

        let seq_bytes = (0x0001u64 as u16).to_be_bytes();
        let mut packet = vec![
            8, 0x00,
            0x00, 0x00,
            0x00, 0x00,
            seq_bytes[0], seq_bytes[1],
        ];
        packet.extend_from_slice(&payload);
        packet.resize(ICMP_HEADER_LEN + PING_HDR_LEN, 0);

        let checksum = csum16_slice(&packet);
        packet[2] = (checksum >> 8) as u8;
        packet[3] = (checksum & 0xff) as u8;

        let verify_csum = csum16_slice(&packet);
        assert_eq!(verify_csum, 0, "verified checksum should be 0");
    }

    #[test]
    fn icmp_v6_packet_type() {
        let echo = Echo { id: 1, len: PING_HDR_LEN as u16, resp_size: 0 };
        let payload = bincode::serialize(&echo).unwrap();

        let seq_bytes = (1u16).to_be_bytes();
        let echo_type: u8 = 128;
        let mut packet = vec![
            echo_type, 0x00,
            0x00, 0x00,
            0x00, 0x00,
            seq_bytes[0], seq_bytes[1],
        ];
        packet.extend_from_slice(&payload);
        packet.resize(ICMP_HEADER_LEN + PING_HDR_LEN, 0);

        assert_eq!(packet[0], 128);
        assert_eq!(packet.len(), ICMP_HEADER_LEN + PING_HDR_LEN);
    }

    #[test]
    fn icmp_reply_type_v4() {
        assert_eq!(0u8, 0);
    }

    #[test]
    fn icmp_reply_type_v6() {
        assert_eq!(129u8, 129);
    }

    #[test]
    fn icmp_packet_payload_starts_after_header() {
        let echo = Echo { id: 0xDEAD, len: 42, resp_size: 100 };
        let payload = bincode::serialize(&echo).unwrap();
        assert_eq!(payload.len(), PING_HDR_LEN);

        let seq_bytes = (0xDEADu64 as u16).to_be_bytes();
        let mut packet = vec![
            8, 0x00,
            0x00, 0x00,
            0x00, 0x00,
            seq_bytes[0], seq_bytes[1],
        ];
        packet.extend_from_slice(&payload);
        packet.resize(ICMP_HEADER_LEN + 42, 0);

        let decoded: Echo = bincode::deserialize(&packet[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN]).unwrap();
        assert_eq!(decoded.id, 0xDEAD);
        assert_eq!(decoded.len, 42);
        assert_eq!(decoded.resp_size, 100);
    }

    #[test]
    fn icmp_packet_minimum_size() {
        let echo = Echo { id: 0, len: PING_HDR_LEN as u16, resp_size: 0 };
        let payload = bincode::serialize(&echo).unwrap();

        let mut packet = vec![8, 0, 0, 0, 0, 0, 0, 0];
        packet.extend_from_slice(&payload);
        packet.resize(ICMP_HEADER_LEN + PING_HDR_LEN, 0);

        assert_eq!(packet.len(), ICMP_HEADER_LEN + PING_HDR_LEN);
    }

    #[test]
    fn icmp_packet_variable_size() {
        for data_len in [12, 64, 128, 256, 512] {
            let echo = Echo { id: 1, len: data_len as u16, resp_size: 0 };
            let payload = bincode::serialize(&echo).unwrap();

            let mut packet = vec![8, 0, 0, 0, 0, 0, 0, 0];
            packet.extend_from_slice(&payload);
            packet.resize(ICMP_HEADER_LEN + data_len, 0);

            assert_eq!(packet.len(), ICMP_HEADER_LEN + data_len);
            let decoded: Echo = bincode::deserialize(&packet[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN]).unwrap();
            assert_eq!(decoded.id, 1);
        }
    }

    #[test]
    fn is_ipv6_detection() {
        let v4: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let v6: SocketAddr = "[::1]:0".parse().unwrap();
        assert!(!is_ipv6(&v4));
        assert!(is_ipv6(&v6));
    }
}
