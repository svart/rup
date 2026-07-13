use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

use crate::TrafficClass;
use crate::echo_codec;
use crate::pinger::{Echo, PING_HDR_LEN, PacketSize, Request, Response};
use crate::tos as traffic;
use crate::transport::Transport;

const ICMP_HEADER_LEN: usize = 8;
const DATA_OFFSET: usize = ICMP_HEADER_LEN;
const ICMP_CODE_ECHO: u8 = 0;
const ICMPV4_ECHO_REQUEST: u8 = 8;
const ICMPV4_ECHO_REPLY: u8 = 0;
const ICMPV6_ECHO_REQUEST: u8 = 128;
const ICMPV6_ECHO_REPLY: u8 = 129;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpVersion {
    V4,
    V6,
}

impl IpVersion {
    fn from_addr(addr: SocketAddr) -> Self {
        if addr.is_ipv4() { Self::V4 } else { Self::V6 }
    }

    fn echo_request_type(self) -> u8 {
        match self {
            Self::V4 => ICMPV4_ECHO_REQUEST,
            Self::V6 => ICMPV6_ECHO_REQUEST,
        }
    }

    fn echo_reply_type(self) -> u8 {
        match self {
            Self::V4 => ICMPV4_ECHO_REPLY,
            Self::V6 => ICMPV6_ECHO_REPLY,
        }
    }

    fn socket_domain_and_protocol(self) -> (Domain, Protocol) {
        match self {
            Self::V4 => (Domain::IPV4, Protocol::ICMPV4),
            Self::V6 => (Domain::IPV6, Protocol::ICMPV6),
        }
    }
}

pub fn build_icmp_packet(req: &Request, ip_version: IpVersion) -> io::Result<Vec<u8>> {
    let r = Echo {
        id: req.id,
        len: req
            .request_size
            .map(PacketSize::get)
            .unwrap_or(PING_HDR_LEN as u16),
        resp_size: req.response_size.map(PacketSize::get).unwrap_or(0),
    };

    let seq_bytes = (req.id as u16).to_be_bytes();

    let mut packet = vec![
        ip_version.echo_request_type(),
        ICMP_CODE_ECHO,
        0x00,
        0x00,
        0x00,
        0x00,
        seq_bytes[0],
        seq_bytes[1],
    ];

    let payload = echo_codec::encode_echo(&r, PING_HDR_LEN);

    let data_len = req
        .request_size
        .map(PacketSize::get)
        .unwrap_or(PING_HDR_LEN as u16) as usize;
    packet.extend_from_slice(&payload);
    packet.resize(ICMP_HEADER_LEN + data_len, 0);

    if ip_version == IpVersion::V4 {
        let checksum = csum16_slice(&packet);
        packet[2] = (checksum >> 8) as u8;
        packet[3] = (checksum & 0xff) as u8;
    }

    Ok(packet)
}

pub fn try_parse_icmp_response(
    packet: &[u8],
    ip_version: IpVersion,
    ttl: Option<u8>,
) -> io::Result<Option<Response>> {
    if packet.len() < DATA_OFFSET + PING_HDR_LEN {
        return Ok(None);
    }
    if packet[0] != ip_version.echo_reply_type() || packet[1] != ICMP_CODE_ECHO {
        return Ok(None);
    }
    let echo: Echo =
        match echo_codec::decode_header(&packet[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN]) {
            Ok(e) => e,
            Err(_) => return Ok(None),
        };
    Ok(Some(Response {
        id: echo.id,
        timestamp: Instant::now(),
        size: packet.len() - DATA_OFFSET,
        ttl,
    }))
}

#[derive(Clone)]
pub struct IcmpClientTransport {
    sock: Arc<UdpSocket>,
    remote: SocketAddr,
    ip_version: IpVersion,
}

impl IcmpClientTransport {
    pub async fn new(local: SocketAddr, remote: SocketAddr) -> io::Result<Self> {
        Self::new_with_tos(local, remote, None).await
    }

    pub async fn new_with_tos(
        local: SocketAddr,
        remote: SocketAddr,
        tos: Option<TrafficClass>,
    ) -> io::Result<Self> {
        let ip_version = IpVersion::from_addr(remote);
        let (domain, protocol) = ip_version.socket_domain_and_protocol();

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
        if let Some(tos_value) = tos {
            traffic::set_socket_tos(&sock, remote, tos_value).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("set TOS {} for {remote} failed: {e}", tos_value.as_u8()),
                )
            })?;
        }
        if let Err(e) = enable_socket_recv_ttl(&sock, remote) {
            eprintln!("ICMP received TTL disabled: {e}");
        }
        sock.set_nonblocking(true)
            .map_err(|e| io::Error::new(e.kind(), format!("set nonblocking: {e}")))?;

        let udp: std::net::UdpSocket = sock.into();
        let sock = UdpSocket::from_std(udp)
            .map_err(|e| io::Error::new(e.kind(), format!("into async socket: {e}")))?;

        Ok(IcmpClientTransport {
            sock: Arc::new(sock),
            remote,
            ip_version,
        })
    }
}

impl Transport for IcmpClientTransport {
    async fn send(&self, req: &Request) -> io::Result<Instant> {
        let packet = build_icmp_packet(req, self.ip_version)?;
        let ts = Instant::now();
        self.sock.send_to(&packet, self.remote).await?;
        Ok(ts)
    }

    async fn recv(&self) -> io::Result<Response> {
        let mut buf = vec![0; u16::MAX as usize];

        loop {
            let (n, addr, ttl) = match recv_from_with_ttl(&self.sock, &mut buf).await {
                Ok(r) => r,
                Err(_) => continue,
            };

            if addr.ip() != self.remote.ip() {
                continue;
            }

            if let Some(resp) = try_parse_icmp_response(&buf[..n], self.ip_version, ttl)? {
                return Ok(resp);
            }
        }
    }
}

#[cfg(unix)]
fn enable_socket_recv_ttl(socket: &Socket, addr: SocketAddr) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let enabled: libc::c_int = 1;
    let (level, optname) = if addr.is_ipv4() {
        (libc::IPPROTO_IP, libc::IP_RECVTTL)
    } else {
        (libc::IPPROTO_IPV6, libc::IPV6_RECVHOPLIMIT)
    };

    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            level,
            optname,
            (&enabled as *const libc::c_int).cast(),
            std::mem::size_of_val(&enabled) as libc::socklen_t,
        )
    };

    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn enable_socket_recv_ttl(_socket: &Socket, _addr: SocketAddr) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
async fn recv_from_with_ttl(
    socket: &UdpSocket,
    buf: &mut [u8],
) -> io::Result<(usize, SocketAddr, Option<u8>)> {
    use std::mem;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
    use std::os::fd::AsRawFd;

    use tokio::io::Interest;

    loop {
        socket.readable().await?;

        let fd = socket.as_raw_fd();
        match socket.try_io(Interest::READABLE, || {
            let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
            let mut control = [0u8; 128];
            let mut iov = libc::iovec {
                iov_base: buf.as_mut_ptr().cast(),
                iov_len: buf.len(),
            };
            let mut msg: libc::msghdr = unsafe { mem::zeroed() };
            msg.msg_name = (&mut storage as *mut libc::sockaddr_storage).cast();
            msg.msg_namelen = mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = control.len() as _;

            let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }

            let addr = match storage.ss_family as libc::c_int {
                libc::AF_INET
                    if msg.msg_namelen as usize >= mem::size_of::<libc::sockaddr_in>() =>
                {
                    let addr =
                        unsafe { *(std::ptr::addr_of!(storage).cast::<libc::sockaddr_in>()) };
                    SocketAddr::V4(SocketAddrV4::new(
                        Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr)),
                        u16::from_be(addr.sin_port),
                    ))
                }
                libc::AF_INET6
                    if msg.msg_namelen as usize >= mem::size_of::<libc::sockaddr_in6>() =>
                {
                    let addr =
                        unsafe { *(std::ptr::addr_of!(storage).cast::<libc::sockaddr_in6>()) };
                    SocketAddr::V6(SocketAddrV6::new(
                        Ipv6Addr::from(addr.sin6_addr.s6_addr),
                        u16::from_be(addr.sin6_port),
                        addr.sin6_flowinfo,
                        addr.sin6_scope_id,
                    ))
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "recvmsg returned an unsupported socket address",
                    ));
                }
            };

            let ttl = unsafe { parse_ttl_cmsg(&msg) };
            Ok((n as usize, addr, ttl))
        }) {
            Ok(result) => return Ok(result),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(not(unix))]
async fn recv_from_with_ttl(
    socket: &UdpSocket,
    buf: &mut [u8],
) -> io::Result<(usize, SocketAddr, Option<u8>)> {
    let (n, addr) = socket.recv_from(buf).await?;
    Ok((n, addr, None))
}

#[cfg(unix)]
unsafe fn parse_ttl_cmsg(msg: &libc::msghdr) -> Option<u8> {
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(msg) };
    while !cmsg.is_null() {
        let level = unsafe { (*cmsg).cmsg_level };
        let ty = unsafe { (*cmsg).cmsg_type };
        if (level == libc::IPPROTO_IP && (ty == libc::IP_TTL || ty == libc::IP_RECVTTL))
            || (level == libc::IPPROTO_IPV6 && ty == libc::IPV6_HOPLIMIT)
        {
            let data = unsafe { libc::CMSG_DATA(cmsg) };
            let data_len = unsafe { (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize };
            if data_len >= std::mem::size_of::<libc::c_int>() {
                let value = unsafe { std::ptr::read_unaligned(data.cast::<libc::c_int>()) };
                return u8::try_from(value).ok();
            }
            if data_len >= 1 {
                return Some(unsafe { *data });
            }
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(msg, cmsg) };
    }
    None
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
    fn build_icmp_v4_packet_structure() {
        let req = Request {
            id: 0xABCD,
            request_size: Some(PacketSize::new(PING_HDR_LEN as u16).unwrap()),
            response_size: None,
        };
        let packet = build_icmp_packet(&req, IpVersion::V4).unwrap();

        assert_eq!(packet[0], ICMPV4_ECHO_REQUEST, "type = echo request");
        assert_eq!(packet[1], ICMP_CODE_ECHO, "code = 0");
        assert_eq!(packet.len(), ICMP_HEADER_LEN + PING_HDR_LEN);
        assert_eq!(packet[6], 0xAB);
        assert_eq!(packet[7], 0xCD);

        let echo =
            echo_codec::decode_header(&packet[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 0xABCD);
        assert_eq!(echo.len, PING_HDR_LEN as u16);
        assert_eq!(echo.resp_size, 0);
    }

    #[test]
    fn build_icmp_v4_checksum_valid() {
        let req = Request {
            id: 1,
            request_size: None,
            response_size: None,
        };
        let packet = build_icmp_packet(&req, IpVersion::V4).unwrap();

        let verify = csum16_slice(&packet);
        assert_eq!(verify, 0, "verified checksum must be zero");
    }

    #[test]
    fn build_icmp_v6_no_checksum_in_packet() {
        let req = Request {
            id: 1,
            request_size: None,
            response_size: None,
        };
        let packet = build_icmp_packet(&req, IpVersion::V6).unwrap();

        assert_eq!(packet[0], ICMPV6_ECHO_REQUEST, "type = echo request v6");
        assert_eq!(packet[2], 0, "no checksum set for v6");
        assert_eq!(packet[3], 0);
    }

    #[test]
    fn build_icmp_packet_variable_sizes() {
        for size in [PING_HDR_LEN as u16, 64, 128, 256, 512] {
            let req = Request {
                id: 10,
                request_size: Some(PacketSize::new(size).unwrap()),
                response_size: None,
            };
            let packet = build_icmp_packet(&req, IpVersion::V4).unwrap();
            assert_eq!(packet.len(), ICMP_HEADER_LEN + size as usize);

            let verify = csum16_slice(&packet);
            assert_eq!(verify, 0, "checksum valid for size={size}");

            let echo = echo_codec::decode_header(&packet[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN])
                .unwrap();
            assert_eq!(echo.id, 10);
            assert_eq!(echo.len, size);
        }
    }

    #[test]
    fn build_icmp_packet_with_resp_size() {
        let req = Request {
            id: 42,
            request_size: Some(PacketSize::new(100).unwrap()),
            response_size: Some(PacketSize::new(200).unwrap()),
        };
        let packet = build_icmp_packet(&req, IpVersion::V4).unwrap();

        let echo =
            echo_codec::decode_header(&packet[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 42);
        assert_eq!(echo.len, 100);
        assert_eq!(echo.resp_size, 200);
        assert_eq!(packet.len(), ICMP_HEADER_LEN + 100);
    }

    #[test]
    fn try_parse_icmp_matching_reply() {
        let req = Request {
            id: 7,
            request_size: None,
            response_size: None,
        };
        let send_pkt = build_icmp_packet(&req, IpVersion::V4).unwrap();

        let mut reply = send_pkt.clone();
        reply[0] = ICMPV4_ECHO_REPLY;

        let result = try_parse_icmp_response(&reply, IpVersion::V4, Some(64)).unwrap();
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.id, 7);
        assert_eq!(result.size, PING_HDR_LEN);
        assert_eq!(result.ttl, Some(64));
    }

    #[test]
    fn try_parse_icmp_wrong_type() {
        let req = Request {
            id: 3,
            request_size: None,
            response_size: None,
        };
        let send_pkt = build_icmp_packet(&req, IpVersion::V4).unwrap();

        let mut reply = send_pkt.clone();
        reply[0] = 3;
        reply[1] = ICMP_CODE_ECHO;

        let result = try_parse_icmp_response(&reply, IpVersion::V4, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_parse_icmp_wrong_code() {
        let req = Request {
            id: 5,
            request_size: None,
            response_size: None,
        };
        let send_pkt = build_icmp_packet(&req, IpVersion::V4).unwrap();

        let mut reply = send_pkt.clone();
        reply[1] = 1;

        let result = try_parse_icmp_response(&reply, IpVersion::V4, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_parse_icmp_too_short() {
        let result = try_parse_icmp_response(&[0u8; 4], IpVersion::V4, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_parse_icmp_just_below_minimum() {
        let buf = vec![0u8; DATA_OFFSET + PING_HDR_LEN - 1];
        let result = try_parse_icmp_response(&buf, IpVersion::V4, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn try_parse_icmp_any_valid_bytes_accepted() {
        let mut buf = vec![0xffu8; DATA_OFFSET + PING_HDR_LEN];
        buf[0] = ICMPV4_ECHO_REPLY;
        buf[1] = ICMP_CODE_ECHO;
        let result = try_parse_icmp_response(&buf, IpVersion::V4, None).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, u64::MAX);
    }

    #[test]
    fn try_parse_icmp_v6_reply_type() {
        let req = Request {
            id: 10,
            request_size: None,
            response_size: None,
        };
        let send_pkt = build_icmp_packet(&req, IpVersion::V6).unwrap();

        let mut reply = send_pkt.clone();
        reply[0] = ICMPV6_ECHO_REPLY;

        let result = try_parse_icmp_response(&reply, IpVersion::V6, None).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, 10);
    }

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

    #[tokio::test]
    async fn icmp_transport_ping_loopback() {
        let local: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let remote: SocketAddr = "127.0.0.1:0".parse().unwrap();

        let transport = match IcmpClientTransport::new(local, remote).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("ICMP socket not available, skipping test: {e}");
                return;
            }
        };

        let req = Request {
            id: 100,
            request_size: None,
            response_size: None,
        };

        transport.send(&req).await.unwrap();

        let resp = tokio::time::timeout(std::time::Duration::from_secs(2), transport.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(resp.id, 100);
    }

    #[tokio::test]
    async fn icmp_transport_ping_loopback_padded() {
        let local: SocketAddr = "0.0.0.0:0".parse().unwrap();
        let remote: SocketAddr = "127.0.0.1:0".parse().unwrap();

        let transport = match IcmpClientTransport::new(local, remote).await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("ICMP socket not available, skipping test: {e}");
                return;
            }
        };

        let req = Request {
            id: 200,
            request_size: Some(PacketSize::new(64).unwrap()),
            response_size: None,
        };

        transport.send(&req).await.unwrap();

        let resp = tokio::time::timeout(std::time::Duration::from_secs(2), transport.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(resp.id, 200);
    }
}
