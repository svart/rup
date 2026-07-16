use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::time::timeout;

use crate::TrafficClass;
use crate::echo_codec;
use crate::pinger::{Echo, PING_HDR_LEN, Request, Response};
use crate::tos as traffic;
use crate::transport::Transport;

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const PROBE_RESPONSE_CHANNEL_CAP: usize = 1024;

enum TcpServerReadError {
    Closed,
    Header(io::Error),
    Payload(io::Error),
    Decode(io::Error),
}

async fn read_tcp_request(sock: &mut TcpStream) -> Result<Echo, TcpServerReadError> {
    let mut hdr_buf = [0; PING_HDR_LEN];
    sock.read_exact(&mut hdr_buf).await.map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            TcpServerReadError::Closed
        } else {
            TcpServerReadError::Header(e)
        }
    })?;

    let req = echo_codec::decode_header(&hdr_buf).map_err(TcpServerReadError::Decode)?;

    if req.len as usize > PING_HDR_LEN {
        let mut extra = vec![0; req.len as usize - PING_HDR_LEN];
        sock.read_exact(&mut extra).await.map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                TcpServerReadError::Closed
            } else {
                TcpServerReadError::Payload(e)
            }
        })?;
    }

    Ok(req)
}

async fn server_connection_handler(mut sock: TcpStream) {
    let peer_addr = match sock.peer_addr() {
        Ok(a) => a,
        Err(_) => return,
    };
    println!("New TCP connection from {peer_addr}");

    loop {
        let req = match read_tcp_request(&mut sock).await {
            Ok(req) => req,
            Err(TcpServerReadError::Closed) => {
                println!("Connection closed: {peer_addr}");
                break;
            }
            Err(TcpServerReadError::Header(e)) => {
                eprintln!("Error reading from {peer_addr}: {e}");
                break;
            }
            Err(TcpServerReadError::Payload(e)) => {
                eprintln!("Error reading request payload from {peer_addr}: {e}");
                break;
            }
            Err(TcpServerReadError::Decode(e)) => {
                eprintln!("Failed to deserialize request from {peer_addr}: {e}");
                break;
            }
        };

        let send_buf = echo_codec::encode_response(req);

        if let Err(e) = sock.write_all(&send_buf).await {
            eprintln!("Error sending echo to {peer_addr}: {e}");
            break;
        }
    }
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
    println!("Running TCP server listening {local_address}");
    let listen_sock = TcpListener::bind(local_address).await.map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("server bind to {local_address} failed: {e}"),
        )
    })?;
    tokio::pin!(shutdown);

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
            _ = &mut shutdown => {
                println!("TCP server shutting down");
                return Ok(());
            }
        }
    }
}

#[derive(Clone)]
struct EchoConnection {
    reader: Arc<Mutex<OwnedReadHalf>>,
    writer: Arc<Mutex<OwnedWriteHalf>>,
}

impl EchoConnection {
    fn new(stream: TcpStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: Arc::new(Mutex::new(reader)),
            writer: Arc::new(Mutex::new(writer)),
        }
    }
}

enum AutoTcpMode {
    Undecided(Option<tokio::net::TcpSocket>),
    Echo(EchoConnection),
    Probe,
}

struct AutoTcpTransport {
    local: SocketAddr,
    remote: SocketAddr,
    tos: Option<TrafficClass>,
    wait_time: Duration,
    mode: Mutex<AutoTcpMode>,
    mode_changed: Notify,
    responses_send: mpsc::Sender<Response>,
    responses_recv: Mutex<mpsc::Receiver<Response>>,
}

enum TcpClientKind {
    Echo(EchoConnection),
    Auto(Box<AutoTcpTransport>),
}

#[derive(Clone)]
pub struct TcpClientTransport {
    kind: Arc<TcpClientKind>,
}

fn configured_tcp_socket(
    local: SocketAddr,
    remote: SocketAddr,
    tos: Option<TrafficClass>,
) -> io::Result<tokio::net::TcpSocket> {
    let socket = if remote.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    socket.bind(local)?;
    if let Some(tos_value) = tos {
        traffic::set_tcp_tos(&socket, remote, tos_value)?;
    }
    Ok(socket)
}

impl TcpClientTransport {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            kind: Arc::new(TcpClientKind::Echo(EchoConnection::new(stream))),
        }
    }

    pub async fn connect(
        local: SocketAddr,
        remote: SocketAddr,
        tos: Option<TrafficClass>,
    ) -> io::Result<Self> {
        let stream = configured_tcp_socket(local, remote, tos)?
            .connect(remote)
            .await?;
        Ok(Self::new(stream))
    }

    /// Creates a transport that selects application echo or TCP connect probes.
    ///
    /// The first request attempts to connect. A successful connection keeps the
    /// regular rup echo protocol; a completed connection error switches all
    /// requests in this transport to independent connect probes.
    pub async fn connect_or_probe(
        local: SocketAddr,
        remote: SocketAddr,
        tos: Option<TrafficClass>,
        wait_time: Duration,
    ) -> io::Result<Self> {
        let initial_socket = configured_tcp_socket(local, remote, tos)?;
        let (responses_send, responses_recv) = mpsc::channel(PROBE_RESPONSE_CHANNEL_CAP);
        Ok(Self {
            kind: Arc::new(TcpClientKind::Auto(Box::new(AutoTcpTransport {
                local,
                remote,
                tos,
                wait_time,
                mode: Mutex::new(AutoTcpMode::Undecided(Some(initial_socket))),
                mode_changed: Notify::new(),
                responses_send,
                responses_recv: Mutex::new(responses_recv),
            }))),
        })
    }
}

impl Transport for TcpClientTransport {
    async fn send(&self, req: &Request) -> io::Result<Instant> {
        match self.kind.as_ref() {
            TcpClientKind::Echo(connection) => send_echo_request(&connection.writer, req).await,
            TcpClientKind::Auto(transport) => send_auto_request(transport, req).await,
        }
    }

    async fn recv(&self) -> io::Result<Response> {
        match self.kind.as_ref() {
            TcpClientKind::Echo(connection) => recv_echo_response(&connection.reader).await,
            TcpClientKind::Auto(transport) => recv_auto_response(transport).await,
        }
    }
}

async fn send_echo_request(
    writer: &Arc<Mutex<OwnedWriteHalf>>,
    req: &Request,
) -> io::Result<Instant> {
    let send_buf = echo_codec::encode_request(req);
    let mut writer = writer.lock().await;
    timeout(IO_TIMEOUT, writer.write_all(&send_buf))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "send timeout"))?
        .map_err(|e| io::Error::new(e.kind(), format!("send failed: {e}")))?;

    Ok(Instant::now())
}

async fn recv_echo_response(reader: &Arc<Mutex<OwnedReadHalf>>) -> io::Result<Response> {
    let mut reader = reader.lock().await;
    let mut hdr = [0; PING_HDR_LEN];
    timeout(IO_TIMEOUT, reader.read_exact(&mut hdr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "recv timeout"))?
        .map_err(|e| match e.kind() {
            io::ErrorKind::UnexpectedEof => {
                io::Error::new(io::ErrorKind::ConnectionAborted, "connection closed")
            }
            _ => io::Error::new(e.kind(), format!("recv header failed: {e}")),
        })?;

    let echo = echo_codec::decode_header(&hdr)?;

    if echo.len as usize > PING_HDR_LEN {
        let mut extra = vec![0; echo.len as usize - PING_HDR_LEN];
        timeout(IO_TIMEOUT, reader.read_exact(&mut extra))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "recv payload timeout"))?
            .map_err(|e| match e.kind() {
                io::ErrorKind::UnexpectedEof => io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "connection closed reading payload",
                ),
                _ => io::Error::new(e.kind(), format!("recv payload failed: {e}")),
            })?;
    }

    Ok(Response {
        id: echo.id,
        timestamp: Instant::now(),
        size: echo.len as usize,
        ttl: None,
    })
}

async fn send_auto_request(transport: &AutoTcpTransport, req: &Request) -> io::Result<Instant> {
    let mut mode = transport.mode.lock().await;
    match &mut *mode {
        AutoTcpMode::Echo(connection) => {
            let writer = connection.writer.clone();
            drop(mode);
            send_echo_request(&writer, req).await
        }
        AutoTcpMode::Probe => {
            drop(mode);
            send_connect_probe(transport, req.id).await
        }
        AutoTcpMode::Undecided(initial_socket) => {
            let socket = initial_socket
                .take()
                .expect("initial TCP socket is present");
            let started = Instant::now();
            match timeout(transport.wait_time, socket.connect(transport.remote)).await {
                Ok(Ok(stream)) => {
                    let connection = EchoConnection::new(stream);
                    let timestamp = send_echo_request(&connection.writer, req).await?;
                    *mode = AutoTcpMode::Echo(connection);
                    transport.mode_changed.notify_waiters();
                    Ok(timestamp)
                }
                Ok(Err(_)) => {
                    *mode = AutoTcpMode::Probe;
                    transport.mode_changed.notify_waiters();
                    queue_connect_response(transport, req.id).await?;
                    Ok(started)
                }
                Err(_) => {
                    *mode = AutoTcpMode::Probe;
                    transport.mode_changed.notify_waiters();
                    Ok(started)
                }
            }
        }
    }
}

async fn send_connect_probe(transport: &AutoTcpTransport, id: u64) -> io::Result<Instant> {
    let socket = configured_tcp_socket(transport.local, transport.remote, transport.tos)?;
    let started = Instant::now();
    if timeout(transport.wait_time, socket.connect(transport.remote))
        .await
        .is_ok()
    {
        queue_connect_response(transport, id).await?;
    }
    Ok(started)
}

async fn queue_connect_response(transport: &AutoTcpTransport, id: u64) -> io::Result<()> {
    transport
        .responses_send
        .send(Response {
            id,
            timestamp: Instant::now(),
            size: 0,
            ttl: None,
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "TCP probe receiver closed"))
}

async fn recv_auto_response(transport: &AutoTcpTransport) -> io::Result<Response> {
    loop {
        let mode_changed = transport.mode_changed.notified();
        let echo_reader = {
            let mode = transport.mode.lock().await;
            match &*mode {
                AutoTcpMode::Undecided(_) => None,
                AutoTcpMode::Echo(connection) => Some(connection.reader.clone()),
                AutoTcpMode::Probe => {
                    drop(mode);
                    return transport
                        .responses_recv
                        .lock()
                        .await
                        .recv()
                        .await
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::BrokenPipe, "TCP probe sender closed")
                        });
                }
            }
        };

        if let Some(reader) = echo_reader {
            return recv_echo_response(&reader).await;
        }
        mode_changed.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PacketSize;
    use tokio::io::AsyncReadExt;

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
            id: 99,
            request_size: Some(PacketSize::new(64).unwrap()),
            response_size: None,
        };
        let buf = echo_codec::encode_request(&req);
        assert_eq!(buf.len(), 64);
        assert_eq!(&buf[PING_HDR_LEN..], &[0u8; 64 - PING_HDR_LEN]);
    }

    #[test]
    fn encode_request_with_resp_size() {
        let req = Request {
            id: 5,
            request_size: Some(PacketSize::new(50).unwrap()),
            response_size: Some(PacketSize::new(200).unwrap()),
        };
        let buf = echo_codec::encode_request(&req);
        let echo = echo_codec::decode_header(&buf[..PING_HDR_LEN]).unwrap();
        assert_eq!(echo.id, 5);
        assert_eq!(echo.resp_size, 200);
    }

    #[test]
    fn decode_header_valid() {
        let req = Request {
            id: 42,
            request_size: Some(PacketSize::new(100).unwrap()),
            response_size: Some(PacketSize::new(200).unwrap()),
        };
        let buf = echo_codec::encode_request(&req);
        let mut hdr = [0u8; PING_HDR_LEN];
        hdr.copy_from_slice(&buf[..PING_HDR_LEN]);
        let echo = echo_codec::decode_header(&hdr).unwrap();
        assert_eq!(echo.id, 42);
        assert_eq!(echo.len, 100);
        assert_eq!(echo.resp_size, 200);
    }

    #[test]
    fn decode_header_all_ff_decodes_to_max() {
        let hdr = [0xff; PING_HDR_LEN];
        let echo = echo_codec::decode_header(&hdr).unwrap();
        assert_eq!(echo.id, u64::MAX);
        assert_eq!(echo.len, u16::MAX);
        assert_eq!(echo.resp_size, u16::MAX);
    }

    #[test]
    fn decode_header_zeroes() {
        let hdr = [0u8; PING_HDR_LEN];
        let echo = echo_codec::decode_header(&hdr).unwrap();
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
            request_size: Some(PacketSize::new(PING_HDR_LEN as u16).unwrap()),
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
            let echo = echo_codec::decode_header(&hdr).unwrap();
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
            request_size: Some(PacketSize::new(64).unwrap()),
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
            request_size: Some(PacketSize::new(PING_HDR_LEN as u16).unwrap()),
            response_size: None,
        };
        transport.send(&req).await.unwrap();
        let result = transport.recv().await;
        assert!(result.is_err());

        server_handle.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_connect_or_probe_closed_port_returns_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = listener.local_addr().unwrap();
        drop(listener);

        let transport = TcpClientTransport::connect_or_probe(
            "0.0.0.0:0".parse().unwrap(),
            remote,
            None,
            Duration::from_millis(100),
        )
        .await
        .unwrap();
        let request = Request {
            id: 9,
            request_size: None,
            response_size: None,
        };

        let sent = transport.send(&request).await.unwrap();
        let response = transport.recv().await.unwrap();

        assert_eq!(response.id, 9);
        assert!(response.timestamp >= sent);
        assert_eq!(response.size, 0);
        assert_eq!(response.ttl, None);
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
            let echo = echo_codec::decode_header(&hdr).unwrap();
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
            request_size: Some(PacketSize::new(512).unwrap()),
            response_size: None,
        };
        transport.send(&req).await.unwrap();
        let resp = transport.recv().await.unwrap();
        assert_eq!(resp.id, 7);

        server_handle.await.unwrap();
    }
}
