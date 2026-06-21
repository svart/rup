use std::collections::HashSet;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

use crate::echo_codec;
use crate::pinger::{Echo, PING_HDR_LEN, Request, Response};
use crate::tos as traffic;
use crate::transport::Transport;

pub fn build_udp_echo(req: &Request) -> io::Result<Vec<u8>> {
    echo_codec::encode_request(req)
}

pub fn parse_udp_response(buf: &[u8]) -> io::Result<Response> {
    echo_codec::decode_response(buf)
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

        let send_buf = match echo_codec::encode_response(req) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("server: failed to serialize response: {e}");
                continue;
            }
        };

        if let Some(tos_value) = packet_tos
            && let Err(e) = traffic::set_udp_tos(&sock, addr, tos_value)
        {
            eprintln!("server: failed to reflect TOS {tos_value} to {addr}: {e}");
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
}

impl UdpClientTransport {
    pub async fn new(local: SocketAddr, remote: SocketAddr) -> io::Result<Self> {
        Self::new_with_tos(local, remote, None).await
    }

    pub async fn new_with_tos(
        local: SocketAddr,
        remote: SocketAddr,
        tos: Option<u8>,
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
                    format!("set TOS {tos_value} for {remote} failed: {e}"),
                )
            })?;
        }
        sock.set_nonblocking(true)?;

        let std_sock: std::net::UdpSocket = sock.into();
        let socket = UdpSocket::from_std(std_sock)
            .map_err(|e| io::Error::new(e.kind(), format!("into async socket: {e}")))?;
        socket.connect(remote).await.map_err(|e| {
            io::Error::new(e.kind(), format!("client connect to {remote} failed: {e}"))
        })?;

        Ok(UdpClientTransport {
            socket: Arc::new(socket),
        })
    }
}

impl Transport for UdpClientTransport {
    async fn send(&self, req: &Request) -> io::Result<Instant> {
        let send_buf = build_udp_echo(req)?;
        let timestamp = Instant::now();
        self.socket.send(&send_buf).await?;
        Ok(timestamp)
    }

    async fn recv(&self) -> io::Result<Response> {
        let mut buf = vec![0; u16::MAX as usize];
        let n = self.socket.recv(&mut buf).await?;
        parse_udp_response(&buf[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinger::Request;

    #[test]
    fn build_udp_echo_default_size() {
        let req = Request {
            id: 10,
            request_size: None,
            response_size: None,
        };
        let buf = build_udp_echo(&req).unwrap();
        assert_eq!(buf.len(), PING_HDR_LEN);
        let echo = echo_codec::decode_header(&buf).unwrap();
        assert_eq!(echo.id, 10);
        assert_eq!(echo.len, PING_HDR_LEN as u16);
    }

    #[test]
    fn build_udp_echo_padded() {
        let req = Request {
            id: 42,
            request_size: Some(100),
            response_size: None,
        };
        let buf = build_udp_echo(&req).unwrap();
        assert_eq!(buf.len(), 100);
        let echo = echo_codec::decode_header(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 42);
        assert_eq!(echo.len, 100);
    }

    #[test]
    fn build_udp_echo_with_resp_size() {
        let req = Request {
            id: 7,
            request_size: Some(50),
            response_size: Some(128),
        };
        let buf = build_udp_echo(&req).unwrap();
        assert_eq!(buf.len(), 50);
        let echo = echo_codec::decode_header(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 7);
        assert_eq!(echo.len, 50);
        assert_eq!(echo.resp_size, 128);
    }

    #[test]
    fn build_udp_echo_zero_id() {
        let req = Request {
            id: 0,
            request_size: Some(12),
            response_size: None,
        };
        let buf = build_udp_echo(&req).unwrap();
        let echo = echo_codec::decode_header(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 0);
    }

    #[test]
    fn parse_udp_response_valid() {
        let req = Request {
            id: 99,
            request_size: None,
            response_size: None,
        };
        let buf = build_udp_echo(&req).unwrap();
        let resp = parse_udp_response(&buf).unwrap();
        assert_eq!(resp.id, 99);
    }

    #[test]
    fn parse_udp_response_too_short() {
        let buf = [0u8; 4];
        let result = parse_udp_response(&buf);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn parse_udp_response_any_bytes_decodes() {
        let buf = [0xff; PING_HDR_LEN];
        let resp = parse_udp_response(&buf).unwrap();
        assert_eq!(resp.id, u64::MAX);
    }

    #[test]
    fn parse_udp_response_empty() {
        let result = parse_udp_response(&[]);
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
            request_size: Some(PING_HDR_LEN as u16),
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
            request_size: Some(PING_HDR_LEN as u16),
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
            request_size: Some(100),
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
        crate::tos::set_socket_tos(&client_sock, server_addr, 0xb8)?;
        client_sock.bind(&client_addr.into())?;
        client_sock.set_nonblocking(true)?;
        let client_std: std::net::UdpSocket = client_sock.into();
        let client_sock = UdpSocket::from_std(client_std)?;

        let req = Request {
            id: 99,
            request_size: None,
            response_size: None,
        };
        let packet = build_udp_echo(&req)?;
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

        assert_eq!(reflected_tos, Some(0xb8));
        Ok(())
    }
}
