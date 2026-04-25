use std::collections::HashSet;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::net::UdpSocket;

use crate::pinger::{Echo, Request, Response, PING_HDR_LEN};
use crate::transport::Transport;

pub fn build_udp_echo(req: &Request) -> io::Result<Vec<u8>> {
    let r = Echo {
        id: req.id,
        len: req.request_size.unwrap_or(PING_HDR_LEN as u16),
        resp_size: req.response_size.unwrap_or(0),
    };

    let mut buf = bincode::serialize(&r).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("serialize: {e}"))
    })?;

    if let Some(size) = req.request_size {
        buf.resize(size as usize, 0);
    }

    Ok(buf)
}

pub fn parse_udp_response(buf: &[u8]) -> io::Result<Response> {
    if buf.len() < PING_HDR_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("packet too short: {} bytes", buf.len()),
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

pub async fn server_transport(local_address: SocketAddr) {
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
        let (n, addr) = tokio::select! {
            result = sock.recv_from(&mut buf) => {
                match result {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("server: recv error: {e}");
                        continue;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("UDP server shutting down");
                return;
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

        if req.resp_size > 0 {
            req.len = req.resp_size;
        }
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
pub struct UdpClientTransport {
    socket: Arc<UdpSocket>,
}

impl UdpClientTransport {
    pub async fn new(local: SocketAddr, remote: SocketAddr) -> io::Result<Self> {
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
        let req = Request { id: 10, request_size: None, response_size: None };
        let buf = build_udp_echo(&req).unwrap();
        assert_eq!(buf.len(), PING_HDR_LEN);
        let echo: Echo = bincode::deserialize(&buf).unwrap();
        assert_eq!(echo.id, 10);
        assert_eq!(echo.len, PING_HDR_LEN as u16);
    }

    #[test]
    fn build_udp_echo_padded() {
        let req = Request { id: 42, request_size: Some(100), response_size: None };
        let buf = build_udp_echo(&req).unwrap();
        assert_eq!(buf.len(), 100);
        let echo: Echo = bincode::deserialize(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 42);
        assert_eq!(echo.len, 100);
    }

    #[test]
    fn build_udp_echo_with_resp_size() {
        let req = Request { id: 7, request_size: Some(50), response_size: Some(128) };
        let buf = build_udp_echo(&req).unwrap();
        assert_eq!(buf.len(), 50);
        let echo: Echo = bincode::deserialize(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 7);
        assert_eq!(echo.len, 50);
        assert_eq!(echo.resp_size, 128);
    }

    #[test]
    fn build_udp_echo_zero_id() {
        let req = Request { id: 0, request_size: Some(12), response_size: None };
        let buf = build_udp_echo(&req).unwrap();
        let echo: Echo = bincode::deserialize(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 0);
    }

    #[test]
    fn parse_udp_response_valid() {
        let req = Request { id: 99, request_size: None, response_size: None };
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

        let transport = UdpClientTransport::new(
            "0.0.0.0:0".parse().unwrap(),
            server_addr,
        )
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

        let transport = UdpClientTransport::new(
            "0.0.0.0:0".parse().unwrap(),
            server_addr,
        )
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

        let transport = UdpClientTransport::new(
            "0.0.0.0:0".parse().unwrap(),
            server_addr,
        )
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
}
