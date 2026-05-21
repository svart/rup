use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::echo_codec;
use crate::pinger::{Echo, PING_HDR_LEN, Request, Response};
use crate::transport::Transport;

const IO_TIMEOUT: Duration = Duration::from_secs(30);

pub fn build_tcp_echo(req: &Request) -> io::Result<Vec<u8>> {
    echo_codec::encode_request(req)
}

pub fn parse_tcp_header(hdr: &[u8; PING_HDR_LEN]) -> io::Result<Echo> {
    echo_codec::decode_header(hdr)
}

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
                let req: Echo = match echo_codec::decode_header(&hdr_buf) {
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

                let send_buf = match echo_codec::encode_response(req) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("Failed to serialize response: {e}");
                        break;
                    }
                };

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

pub async fn server_transport(local_address: SocketAddr) -> io::Result<()> {
    println!("Running TCP server listening {local_address}");
    let listen_sock = TcpListener::bind(local_address).await.map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("server bind to {local_address} failed: {e}"),
        )
    })?;

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
                return Ok(());
            }
        }
    }
}

#[derive(Clone)]
pub struct TcpClientTransport {
    stream: Arc<TcpStream>,
}

impl TcpClientTransport {
    pub fn new(stream: TcpStream) -> Self {
        TcpClientTransport {
            stream: Arc::new(stream),
        }
    }
}

impl Transport for TcpClientTransport {
    async fn send(&self, req: &Request) -> io::Result<Instant> {
        let send_buf = build_tcp_echo(req)?;
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
                Err(e) => return Err(io::Error::new(e.kind(), format!("recv header failed: {e}"))),
            }
        }

        let echo = parse_tcp_header(&hdr)?;

        if echo.len as usize > PING_HDR_LEN {
            let remaining = echo.len as usize - PING_HDR_LEN;
            let mut extra = vec![0; remaining];
            let mut offset = 0;
            while offset < remaining {
                timeout(IO_TIMEOUT, self.stream.readable())
                    .await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "recv payload timeout"))?
                    .map_err(|e| io::Error::new(e.kind(), format!("recv: readable failed: {e}")))?;

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
                        ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn build_tcp_echo_default_size() {
        let req = Request {
            id: 10,
            request_size: None,
            response_size: None,
        };
        let buf = build_tcp_echo(&req).unwrap();
        assert_eq!(buf.len(), PING_HDR_LEN);
        let echo: Echo = bincode::deserialize(&buf).unwrap();
        assert_eq!(echo.id, 10);
        assert_eq!(echo.len, PING_HDR_LEN as u16);
    }

    #[test]
    fn build_tcp_echo_padded() {
        let req = Request {
            id: 99,
            request_size: Some(64),
            response_size: None,
        };
        let buf = build_tcp_echo(&req).unwrap();
        assert_eq!(buf.len(), 64);
        assert_eq!(&buf[PING_HDR_LEN..], &[0u8; 64 - PING_HDR_LEN]);
    }

    #[test]
    fn build_tcp_echo_with_resp_size() {
        let req = Request {
            id: 5,
            request_size: Some(50),
            response_size: Some(200),
        };
        let buf = build_tcp_echo(&req).unwrap();
        let echo: Echo = bincode::deserialize(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 5);
        assert_eq!(echo.resp_size, 200);
    }

    #[test]
    fn parse_tcp_header_valid() {
        let req = Request {
            id: 42,
            request_size: Some(100),
            response_size: Some(200),
        };
        let buf = build_tcp_echo(&req).unwrap();
        let mut hdr = [0u8; PING_HDR_LEN];
        hdr.copy_from_slice(&buf[..PING_HDR_LEN]);
        let echo = parse_tcp_header(&hdr).unwrap();
        assert_eq!(echo.id, 42);
        assert_eq!(echo.len, 100);
        assert_eq!(echo.resp_size, 200);
    }

    #[test]
    fn parse_tcp_header_all_ff_decodes_to_max() {
        let hdr = [0xff; PING_HDR_LEN];
        let echo = parse_tcp_header(&hdr).unwrap();
        assert_eq!(echo.id, u64::MAX);
        assert_eq!(echo.len, u16::MAX);
        assert_eq!(echo.resp_size, u16::MAX);
    }

    #[test]
    fn parse_tcp_header_partial() {
        let hdr = [0u8; PING_HDR_LEN];
        let echo = parse_tcp_header(&hdr).unwrap();
        assert_eq!(echo.id, 0);
        assert_eq!(echo.len, 0);
        assert_eq!(echo.resp_size, 0);
    }

    #[tokio::test]
    async fn tcp_transport_send_and_receive() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        let client_stream = TcpStream::connect(server_addr).await.unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();

        let transport = TcpClientTransport::new(client_stream);

        let server_handle = tokio::spawn(async move {
            let mut hdr = [0; PING_HDR_LEN];
            server_stream.read_exact(&mut hdr).await.unwrap();
            let _ = server_stream.write_all(&hdr).await;
        });

        let req = Request {
            id: 42,
            request_size: Some(PING_HDR_LEN as u16),
            response_size: None,
        };
        transport.send(&req).await.unwrap();
        let resp = transport.recv().await.unwrap();
        assert_eq!(resp.id, 42);

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_transport_send_with_padding() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        let client_stream = TcpStream::connect(server_addr).await.unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();

        let transport = TcpClientTransport::new(client_stream);

        let server_handle = tokio::spawn(async move {
            let mut hdr = [0; PING_HDR_LEN];
            server_stream.read_exact(&mut hdr).await.unwrap();
            let echo: Echo = bincode::deserialize(&hdr).unwrap();
            let remaining = echo.len as usize - PING_HDR_LEN;
            if remaining > 0 {
                let mut extra = vec![0; remaining];
                server_stream.read_exact(&mut extra).await.unwrap();
            }
            let mut send_buf = hdr.to_vec();
            send_buf.resize(echo.len as usize, 0);
            let _ = server_stream.write_all(&send_buf).await;
        });

        let req = Request {
            id: 99,
            request_size: Some(64),
            response_size: None,
        };
        transport.send(&req).await.unwrap();
        let resp = transport.recv().await.unwrap();
        assert_eq!(resp.id, 99);

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_transport_recv_connection_closed() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        let client_stream = TcpStream::connect(server_addr).await.unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();

        let transport = TcpClientTransport::new(client_stream);

        let server_handle = tokio::spawn(async move {
            let mut hdr = [0; PING_HDR_LEN];
            server_stream.read_exact(&mut hdr).await.unwrap();
            drop(server_stream);
        });

        let req = Request {
            id: 0,
            request_size: Some(PING_HDR_LEN as u16),
            response_size: None,
        };
        transport.send(&req).await.unwrap();
        let result = transport.recv().await;
        assert!(result.is_err());

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_transport_send_large_request() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        let client_stream = TcpStream::connect(server_addr).await.unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();

        let transport = TcpClientTransport::new(client_stream);

        let server_handle = tokio::spawn(async move {
            let mut hdr = [0; PING_HDR_LEN];
            server_stream.read_exact(&mut hdr).await.unwrap();
            let echo: Echo = bincode::deserialize(&hdr).unwrap();
            let remaining = echo.len as usize - PING_HDR_LEN;
            if remaining > 0 {
                let mut extra = vec![0; remaining];
                server_stream.read_exact(&mut extra).await.unwrap();
            }
            let mut send_buf = hdr.to_vec();
            send_buf.resize(echo.len as usize, 0);
            let _ = server_stream.write_all(&send_buf).await;
        });

        let req = Request {
            id: 7,
            request_size: Some(512),
            response_size: None,
        };
        transport.send(&req).await.unwrap();
        let resp = transport.recv().await.unwrap();
        assert_eq!(resp.id, 7);

        server_handle.await.unwrap();
    }
}
