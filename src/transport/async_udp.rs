use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
#[cfg(target_os = "linux")]
use tokio::sync::{Mutex, mpsc};

use crate::TrafficClass;
use crate::echo_codec;
use crate::pinger::{Echo, PING_HDR_LEN, Request, Response};
use crate::tos as traffic;
use crate::transport::Transport;

#[cfg(target_os = "linux")]
fn response_from_error_payload(payload: &[u8], origin: u8) -> Option<Response> {
    if !matches!(origin, libc::SO_EE_ORIGIN_ICMP | libc::SO_EE_ORIGIN_ICMP6) {
        return None;
    }

    let echo = echo_codec::decode_header(payload).ok()?;
    Some(Response {
        id: echo.id,
        timestamp: Instant::now(),
        size: 0,
        ttl: None,
    })
}

#[cfg(target_os = "linux")]
fn enable_socket_recv_errors(socket: &Socket, remote: SocketAddr) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let enabled: libc::c_int = 1;
    let (level, option) = if remote.is_ipv4() {
        (libc::IPPROTO_IP, libc::IP_RECVERR)
    } else {
        (libc::IPPROTO_IPV6, libc::IPV6_RECVERR)
    };
    let result = unsafe {
        libc::setsockopt(
            socket.as_raw_fd(),
            level,
            option,
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

#[cfg(target_os = "linux")]
fn recv_socket_error(socket: &UdpSocket, payload: &mut [u8]) -> io::Result<Option<Response>> {
    use std::os::fd::AsRawFd;

    let mut control = [0u8; 128];
    let mut iov = libc::iovec {
        iov_base: payload.as_mut_ptr().cast(),
        iov_len: payload.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control.len();

    let n = unsafe {
        libc::recvmsg(
            socket.as_raw_fd(),
            &mut msg,
            libc::MSG_ERRQUEUE | libc::MSG_DONTWAIT,
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }

    let origin = unsafe { extended_error_origin(&msg) };
    Ok(origin.and_then(|origin| response_from_error_payload(&payload[..n as usize], origin)))
}

#[cfg(target_os = "linux")]
unsafe fn extended_error_origin(msg: &libc::msghdr) -> Option<u8> {
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(msg) };
    while !cmsg.is_null() {
        let level = unsafe { (*cmsg).cmsg_level };
        let kind = unsafe { (*cmsg).cmsg_type };
        if (level == libc::IPPROTO_IP && kind == libc::IP_RECVERR)
            || (level == libc::IPPROTO_IPV6 && kind == libc::IPV6_RECVERR)
        {
            let data_len = unsafe { (*cmsg).cmsg_len as usize }
                .saturating_sub(unsafe { libc::CMSG_LEN(0) } as usize);
            if data_len >= std::mem::size_of::<libc::sock_extended_err>() {
                let error = unsafe {
                    std::ptr::read_unaligned(
                        libc::CMSG_DATA(cmsg).cast::<libc::sock_extended_err>(),
                    )
                };
                return Some(error.ee_origin);
            }
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(msg, cmsg) };
    }
    None
}

pub async fn server_transport(local_address: SocketAddr) -> io::Result<()> {
    server_transport_until(local_address, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

pub async fn server_transport_until(
    local_address: SocketAddr,
    shutdown: impl Future<Output = ()>,
) -> io::Result<()> {
    println!("Running UDP server listening {local_address}");

    let sock = bind_server_socket(local_address).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("server bind to {local_address} failed: {e}"),
        )
    })?;

    let mut client_addrs: HashSet<SocketAddr> = HashSet::new();
    let mut buf = vec![0; u16::MAX as usize];
    tokio::pin!(shutdown);

    loop {
        let (n, addr, packet_tos) = tokio::select! {
            result = traffic::recv_from_with_tos(&sock, &mut buf) => {
                match result {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("server: recv error: {e}");
                        continue;
                    }
                }
            }
            _ = &mut shutdown => {
                println!("UDP server shutting down");
                return Ok(());
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

        let req: Echo = match echo_codec::decode_header(&buf[..n]) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("server: failed to deserialize from {addr}: {e}");
                continue;
            }
        };

        let send_buf = echo_codec::encode_response(req);

        if let Some(tos_value) = packet_tos
            && let Err(e) = traffic::set_udp_tos(&sock, addr, tos_value)
        {
            eprintln!(
                "server: failed to reflect TOS {} to {addr}: {e}",
                tos_value.as_u8()
            );
        }

        if let Err(e) = sock.send_to(&send_buf, addr).await {
            eprintln!("server: send error to {addr}: {e}");
        }
    }
}

fn bind_server_socket(local_address: SocketAddr) -> io::Result<UdpSocket> {
    let domain = if local_address.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    if let Err(e) = traffic::enable_socket_recv_tos(&sock, local_address) {
        eprintln!("server: received TOS reflection disabled: {e}");
    }
    sock.bind(&local_address.into())?;
    sock.set_nonblocking(true)?;

    let std_sock: std::net::UdpSocket = sock.into();
    UdpSocket::from_std(std_sock)
}

#[derive(Clone)]
pub struct UdpClientTransport {
    socket: Arc<UdpSocket>,
    #[cfg(target_os = "linux")]
    queued_errors_send: mpsc::UnboundedSender<Response>,
    #[cfg(target_os = "linux")]
    queued_errors_recv: Arc<Mutex<mpsc::UnboundedReceiver<Response>>>,
}

impl UdpClientTransport {
    pub async fn new(local: SocketAddr, remote: SocketAddr) -> io::Result<Self> {
        Self::new_with_tos(local, remote, None).await
    }

    pub async fn new_with_tos(
        local: SocketAddr,
        remote: SocketAddr,
        tos: Option<TrafficClass>,
    ) -> io::Result<Self> {
        let domain = if remote.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };
        let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        sock.bind(&local.into())
            .map_err(|e| io::Error::new(e.kind(), format!("client bind to {local} failed: {e}")))?;
        if let Some(tos_value) = tos {
            traffic::set_socket_tos(&sock, remote, tos_value).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("set TOS {} for {remote} failed: {e}", tos_value.as_u8()),
                )
            })?;
        }
        #[cfg(target_os = "linux")]
        enable_socket_recv_errors(&sock, remote).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("enable extended socket errors for {remote} failed: {e}"),
            )
        })?;
        sock.set_nonblocking(true)?;

        let std_sock: std::net::UdpSocket = sock.into();
        let socket = UdpSocket::from_std(std_sock)
            .map_err(|e| io::Error::new(e.kind(), format!("into async socket: {e}")))?;
        socket.connect(remote).await.map_err(|e| {
            io::Error::new(e.kind(), format!("client connect to {remote} failed: {e}"))
        })?;

        #[cfg(target_os = "linux")]
        let (queued_errors_send, queued_errors_recv) = mpsc::unbounded_channel();

        Ok(UdpClientTransport {
            socket: Arc::new(socket),
            #[cfg(target_os = "linux")]
            queued_errors_send,
            #[cfg(target_os = "linux")]
            queued_errors_recv: Arc::new(Mutex::new(queued_errors_recv)),
        })
    }
}

impl Transport for UdpClientTransport {
    async fn send(&self, req: &Request) -> io::Result<Instant> {
        let send_buf = echo_codec::encode_request(req);
        loop {
            let timestamp = Instant::now();
            match self.socket.send(&send_buf).await {
                Ok(_) => return Ok(timestamp),
                #[cfg(target_os = "linux")]
                Err(send_error) => {
                    let mut payload = vec![0; u16::MAX as usize];
                    match recv_socket_error(&self.socket, &mut payload) {
                        Ok(Some(response)) => {
                            self.queued_errors_send.send(response).map_err(|_| {
                                io::Error::new(
                                    io::ErrorKind::BrokenPipe,
                                    "UDP error response receiver closed",
                                )
                            })?;
                        }
                        Ok(None) => return Err(send_error),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(send_error),
                        Err(e) => return Err(e),
                    }
                }
                #[cfg(not(target_os = "linux"))]
                Err(send_error) => return Err(send_error),
            }
        }
    }

    async fn recv(&self) -> io::Result<Response> {
        let mut buf = vec![0; u16::MAX as usize];

        #[cfg(target_os = "linux")]
        loop {
            use tokio::io::Interest;

            let queued_response = async {
                self.queued_errors_recv
                    .lock()
                    .await
                    .recv()
                    .await
                    .expect("UDP transport retains error response sender")
            };
            tokio::select! {
                response = queued_response => return Ok(response),
                ready = self.socket.ready(Interest::READABLE | Interest::ERROR) => {
                    let ready = ready?;
                    if ready.is_error() {
                        match self.socket.try_io(Interest::ERROR, || {
                            recv_socket_error(&self.socket, &mut buf)
                        }) {
                            Ok(Some(response)) => return Ok(response),
                            Ok(None) => continue,
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                            Err(e) => return Err(e),
                        }
                    }
                    if ready.is_readable() {
                        match self.socket.try_recv(&mut buf) {
                            Ok(n) => return echo_codec::decode_response(&buf[..n]),
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                            Err(recv_error) => match recv_socket_error(&self.socket, &mut buf) {
                                Ok(Some(response)) => return Ok(response),
                                Ok(None) => return Err(recv_error),
                                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                    return Err(recv_error);
                                }
                                Err(e) => return Err(e),
                            },
                        }
                    }
                }
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let n = self.socket.recv(&mut buf).await?;
            echo_codec::decode_response(&buf[..n])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PacketSize;
    use crate::pinger::Request;

    #[test]
    #[cfg(target_os = "linux")]
    fn remote_icmp_error_payload_becomes_zero_size_response() {
        let payload = echo_codec::encode_request(&Request {
            id: 42,
            request_size: Some(PacketSize::new(64).unwrap()),
            response_size: None,
        });

        let response = response_from_error_payload(&payload, libc::SO_EE_ORIGIN_ICMP).unwrap();

        assert_eq!(response.id, 42);
        assert_eq!(response.size, 0);
        assert_eq!(response.ttl, None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn local_and_malformed_error_payloads_are_ignored() {
        let payload = echo_codec::encode_request(&Request {
            id: 7,
            request_size: None,
            response_size: None,
        });

        assert!(response_from_error_payload(&payload, libc::SO_EE_ORIGIN_LOCAL).is_none());
        assert!(
            response_from_error_payload(&payload[..PING_HDR_LEN - 1], libc::SO_EE_ORIGIN_ICMP)
                .is_none()
        );
    }

    #[test]
    fn encode_request_default_size() {
        let req = Request {
            id: 10,
            request_size: None,
            response_size: None,
        };
        let buf = echo_codec::encode_request(&req);
        assert_eq!(buf.len(), PING_HDR_LEN);
        let echo = echo_codec::decode_header(&buf).unwrap();
        assert_eq!(echo.id, 10);
        assert_eq!(echo.len, PING_HDR_LEN as u16);
    }

    #[test]
    fn encode_request_padded() {
        let req = Request {
            id: 42,
            request_size: Some(PacketSize::new(100).unwrap()),
            response_size: None,
        };
        let buf = echo_codec::encode_request(&req);
        assert_eq!(buf.len(), 100);
        let echo = echo_codec::decode_header(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 42);
        assert_eq!(echo.len, 100);
    }

    #[test]
    fn encode_request_with_resp_size() {
        let req = Request {
            id: 7,
            request_size: Some(PacketSize::new(50).unwrap()),
            response_size: Some(PacketSize::new(128).unwrap()),
        };
        let buf = echo_codec::encode_request(&req);
        assert_eq!(buf.len(), 50);
        let echo = echo_codec::decode_header(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 7);
        assert_eq!(echo.len, 50);
        assert_eq!(echo.resp_size, 128);
    }

    #[test]
    fn encode_request_zero_id() {
        let req = Request {
            id: 0,
            request_size: Some(PacketSize::new(12).unwrap()),
            response_size: None,
        };
        let buf = echo_codec::encode_request(&req);
        let echo = echo_codec::decode_header(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 0);
    }

    #[test]
    fn decode_response_valid() {
        let req = Request {
            id: 99,
            request_size: None,
            response_size: None,
        };
        let buf = echo_codec::encode_request(&req);
        let resp = echo_codec::decode_response(&buf).unwrap();
        assert_eq!(resp.id, 99);
    }

    #[test]
    fn decode_response_too_short() {
        let buf = [0u8; 4];
        let result = echo_codec::decode_response(&buf);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn decode_response_any_bytes_decodes() {
        let buf = [0xff; PING_HDR_LEN];
        let resp = echo_codec::decode_response(&buf).unwrap();
        assert_eq!(resp.id, u64::MAX);
    }

    #[test]
    fn decode_response_empty() {
        let result = echo_codec::decode_response(&[]);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn udp_transport_send_and_receive() {
        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();

        let transport = UdpClientTransport::new("0.0.0.0:0".parse().unwrap(), server_addr)
            .await
            .unwrap();

        let server_handle = tokio::spawn(async move {
            let mut buf = vec![0; PING_HDR_LEN + 100];
            let (n, client_addr) = server_sock.recv_from(&mut buf).await.unwrap();
            let _ = server_sock.send_to(&buf[..n], client_addr).await;
        });

        let req = Request {
            id: 7,
            request_size: Some(PacketSize::new(PING_HDR_LEN as u16).unwrap()),
            response_size: None,
        };
        transport.send(&req).await.unwrap();

        let resp = transport.recv().await.unwrap();
        assert_eq!(resp.id, 7);

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn udp_transport_recv_short_packet_error() {
        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();

        let transport = UdpClientTransport::new("0.0.0.0:0".parse().unwrap(), server_addr)
            .await
            .unwrap();

        let server_handle = tokio::spawn(async move {
            let mut buf = vec![0; PING_HDR_LEN + 100];
            let (_n, client_addr) = server_sock.recv_from(&mut buf).await.unwrap();
            let short = &buf[..4];
            let _ = server_sock.send_to(short, client_addr).await;
        });

        let req = Request {
            id: 1,
            request_size: Some(PacketSize::new(PING_HDR_LEN as u16).unwrap()),
            response_size: None,
        };
        transport.send(&req).await.unwrap();

        let result = transport.recv().await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn udp_transport_send_with_padding() {
        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();

        let transport = UdpClientTransport::new("0.0.0.0:0".parse().unwrap(), server_addr)
            .await
            .unwrap();

        let server_handle = tokio::spawn(async move {
            let mut buf = vec![0; 512];
            let (n, client_addr) = server_sock.recv_from(&mut buf).await.unwrap();
            assert!(n >= 100, "expected padded request >= 100 bytes, got {n}");
            let _ = server_sock.send_to(&buf[..n], client_addr).await;
        });

        let req = Request {
            id: 2,
            request_size: Some(PacketSize::new(100).unwrap()),
            response_size: None,
        };
        transport.send(&req).await.unwrap();

        let resp = transport.recv().await.unwrap();
        assert_eq!(resp.id, 2);

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn udp_transport_new_bind_failure() {
        let result = UdpClientTransport::new(
            "1.2.3.4:9999".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await;
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn udp_server_reflects_received_tos() -> io::Result<()> {
        let reserved = std::net::UdpSocket::bind("127.0.0.1:0")?;
        let server_addr = reserved.local_addr()?;
        drop(reserved);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(server_transport_until(server_addr, async {
            let _ = shutdown_rx.await;
        }));

        let client_sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        let client_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        crate::tos::enable_socket_recv_tos(&client_sock, client_addr)?;
        crate::tos::set_socket_tos(&client_sock, server_addr, TrafficClass::new(0xb8))?;
        client_sock.bind(&client_addr.into())?;
        client_sock.set_nonblocking(true)?;
        let client_std: std::net::UdpSocket = client_sock.into();
        let client_sock = UdpSocket::from_std(client_std)?;

        let req = Request {
            id: 99,
            request_size: None,
            response_size: None,
        };
        let packet = echo_codec::encode_request(&req);
        client_sock.send_to(&packet, server_addr).await?;

        let mut buf = [0u8; 64];
        let (_, _, reflected_tos) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::tos::recv_from_with_tos(&client_sock, &mut buf),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out waiting for response"))??;

        shutdown_tx.send(()).unwrap();
        server.await.unwrap()?;

        assert_eq!(reflected_tos, Some(TrafficClass::new(0xb8)));
        Ok(())
    }
}
