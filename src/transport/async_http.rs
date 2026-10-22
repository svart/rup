use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::client::conn::{http1, http2};
use hyper::header::{HOST, HeaderValue, USER_AGENT};
use hyper::{Uri, http};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};
use tokio::time::timeout;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore, crypto};

use crate::TrafficClass;
use crate::http_target::{HttpScheme, HttpTarget, HttpVersion};
use crate::pinger::{Request, Response};
use crate::transport::Transport;
use crate::transport::async_tcp::configured_tcp_socket;

const RESPONSE_CHANNEL_CAP: usize = 1024;
const USER_AGENT_VALUE: &str = concat!("rup/", env!("CARGO_PKG_VERSION"));

type RequestBody = Empty<Bytes>;

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// Everything needed to open a connection and build a `GET` request.
struct Endpoint {
    version: HttpVersion,
    local: SocketAddr,
    remote: SocketAddr,
    tos: Option<TrafficClass>,
    wait_time: Duration,
    tls: Option<(TlsConnector, ServerName<'static>)>,
    uri: Uri,
    host: HeaderValue,
}

impl Endpoint {
    fn new(
        version: HttpVersion,
        local: SocketAddr,
        remote: SocketAddr,
        tos: Option<TrafficClass>,
        target: &HttpTarget,
        wait_time: Duration,
    ) -> io::Result<Self> {
        let tls = match target.scheme() {
            HttpScheme::Http => None,
            HttpScheme::Https => {
                let server_name = ServerName::try_from(target.host().to_owned())
                    .map_err(|e| invalid_input(format!("invalid TLS server name: {e}")))?;
                Some((tls_connector(version)?, server_name))
            }
        };
        // HTTP/1.1 sends the origin-form path with a Host header; HTTP/2 takes
        // :scheme and :authority from the absolute URI.
        let uri = match version {
            HttpVersion::Http1 => target.path().parse(),
            HttpVersion::Http2 => target.to_string().parse(),
        }
        .map_err(|e| invalid_input(format!("invalid URL {target}: {e}")))?;
        let host = HeaderValue::try_from(target.authority())
            .map_err(|e| invalid_input(format!("invalid host {}: {e}", target.host())))?;

        Ok(Self {
            version,
            local,
            remote,
            tos,
            wait_time,
            tls,
            uri,
            host,
        })
    }

    fn request(&self) -> http::Request<RequestBody> {
        let mut request = http::Request::new(Empty::new());
        *request.uri_mut() = self.uri.clone();
        let headers = request.headers_mut();
        if self.version == HttpVersion::Http1 {
            headers.insert(HOST, self.host.clone());
        }
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        request
    }

    async fn connect(&self) -> io::Result<Box<dyn Stream>> {
        timeout(self.wait_time, self.connect_stream())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("connect to {} timed out", self.remote),
                )
            })?
    }

    async fn connect_stream(&self) -> io::Result<Box<dyn Stream>> {
        let tcp = configured_tcp_socket(self.local, self.remote, self.tos)?
            .connect(self.remote)
            .await
            .map_err(|e| {
                io::Error::new(e.kind(), format!("connect to {} failed: {e}", self.remote))
            })?;
        // Without this, Nagle's algorithm can hold a request behind a small
        // unacknowledged HTTP/2 control frame until the peer's delayed ACK.
        tcp.set_nodelay(true)?;
        let Some((connector, server_name)) = &self.tls else {
            return Ok(Box::new(tcp));
        };

        let tls = connector
            .connect(server_name.clone(), tcp)
            .await
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("TLS handshake with {} failed: {e}", server_name.to_str()),
                )
            })?;
        if self.version == HttpVersion::Http2 && tls.get_ref().1.alpn_protocol() != Some(b"h2") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{} did not negotiate HTTP/2", server_name.to_str()),
            ));
        }
        Ok(Box::new(tls))
    }

    /// Runs one complete HTTP/1.1 exchange on a new connection.
    async fn http1_exchange(&self) -> io::Result<usize> {
        let stream = self.connect().await?;
        let (mut sender, connection) = http1::handshake(TokioIo::new(stream))
            .await
            .map_err(http_error)?;
        // Drive the connection here instead of spawning it, so a cancelled
        // exchange closes the socket immediately.
        let exchange = async {
            let response = sender
                .send_request(self.request())
                .await
                .map_err(http_error)?;
            read_response(response).await
        };
        tokio::select! {
            result = exchange => result,
            Err(error) = connection => Err(http_error(error)),
        }
    }

    async fn http2_connect(&self) -> io::Result<http2::SendRequest<RequestBody>> {
        let stream = self.connect().await?;
        let (sender, connection) = http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
            .await
            .map_err(http_error)?;
        tokio::spawn(connection);
        Ok(sender)
    }
}

/// Converts a hyper error, keeping its causes; hyper's own message is often
/// just a category such as "http2 error".
fn http_error(error: hyper::Error) -> io::Error {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    io::Error::other(message)
}

fn invalid_input(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn tls_connector(version: HttpVersion) -> io::Result<TlsConnector> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut config =
        ClientConfig::builder_with_provider(Arc::new(crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_root_certificates(roots)
            .with_no_client_auth();
    config.alpn_protocols = vec![match version {
        HttpVersion::Http1 => b"http/1.1".to_vec(),
        HttpVersion::Http2 => b"h2".to_vec(),
    }];
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Reads the full response body and returns the response size.
///
/// The status line and headers are counted in HTTP/1.1 text form, so the size
/// is never zero and is comparable between HTTP versions.
async fn read_response(response: hyper::Response<Incoming>) -> io::Result<usize> {
    let status = response.status();
    let status_line = "HTTP/1.1 200 \r\n".len() + status.canonical_reason().unwrap_or("").len();
    let headers: usize = response
        .headers()
        .iter()
        .map(|(name, value)| name.as_str().len() + ": \r\n".len() + value.len())
        .sum();
    let mut size = status_line + headers + "\r\n".len();

    let mut body = response.into_body();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.map_err(http_error)?.into_data() {
            size += data.len();
        }
    }
    Ok(size)
}

type Exchange = Pin<Box<dyn Future<Output = io::Result<usize>> + Send>>;

enum Client {
    Http1,
    Http2(Mutex<http2::SendRequest<RequestBody>>),
}

struct HttpClient {
    endpoint: Endpoint,
    client: Client,
    responses_send: mpsc::Sender<Response>,
    responses_recv: Mutex<mpsc::Receiver<Response>>,
}

impl HttpClient {
    /// Starts one `GET`; returns its start time and a future that completes
    /// with the response size once the full body is read.
    async fn start_exchange(self: &Arc<Self>) -> io::Result<(Instant, Exchange)> {
        match &self.client {
            Client::Http1 => {
                let client = self.clone();
                let started = Instant::now();
                Ok((
                    started,
                    Box::pin(async move { client.endpoint.http1_exchange().await }),
                ))
            }
            Client::Http2(sender) => {
                let mut sender = ready_http2_sender(&self.endpoint, sender).await?;
                let started = Instant::now();
                let response = sender.send_request(self.endpoint.request());
                Ok((
                    started,
                    Box::pin(
                        async move { read_response(response.await.map_err(http_error)?).await },
                    ),
                ))
            }
        }
    }
}

async fn ready_http2_sender(
    endpoint: &Endpoint,
    sender: &Mutex<http2::SendRequest<RequestBody>>,
) -> io::Result<http2::SendRequest<RequestBody>> {
    let mut sender = sender.lock().await;
    if sender.ready().await.is_err() {
        // The server closed the connection, e.g. with GOAWAY after idle time
        // or a request limit.
        *sender = endpoint.http2_connect().await?;
        sender.ready().await.map_err(http_error)?;
    }
    Ok(sender.clone())
}

/// HTTP `GET` transport; RTT ends when the full response body is read.
///
/// HTTP/1.1 opens a new connection for every request, so its RTT includes the
/// TCP connect and TLS handshake. HTTP/2 multiplexes all requests over one
/// persistent connection and reconnects when the server closes it. Failed or
/// timed-out requests produce no response and are reported as lost.
#[derive(Clone)]
pub struct HttpClientTransport {
    inner: Arc<HttpClient>,
}

impl HttpClientTransport {
    /// Sends one unmeasured `GET` so that unreachable targets, TLS errors, and
    /// missing HTTP/2 support fail here instead of as silent packet loss.
    ///
    /// `target` provides the scheme, TLS server name, `Host` header, and path;
    /// `remote` is its resolved address. Any HTTP status counts as success.
    pub async fn connect(
        version: HttpVersion,
        local: SocketAddr,
        remote: SocketAddr,
        tos: Option<TrafficClass>,
        target: HttpTarget,
        wait_time: Duration,
    ) -> io::Result<Self> {
        let endpoint = Endpoint::new(version, local, remote, tos, &target, wait_time)?;
        let client = match version {
            HttpVersion::Http1 => Client::Http1,
            HttpVersion::Http2 => Client::Http2(Mutex::new(endpoint.http2_connect().await?)),
        };
        let (responses_send, responses_recv) = mpsc::channel(RESPONSE_CHANNEL_CAP);
        let inner = Arc::new(HttpClient {
            endpoint,
            client,
            responses_send,
            responses_recv: Mutex::new(responses_recv),
        });

        let (_, exchange) = inner.start_exchange().await?;
        timeout(wait_time, exchange)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "timed out"))
            .flatten()
            .map_err(|e| io::Error::new(e.kind(), format!("GET {target} failed: {e}")))?;
        Ok(Self { inner })
    }
}

impl Transport for HttpClientTransport {
    async fn send(&self, req: &Request) -> io::Result<Instant> {
        let (started, exchange) = self.inner.start_exchange().await?;
        let id = req.id;
        let responses = self.inner.responses_send.clone();
        let wait_time = self.inner.endpoint.wait_time;
        tokio::spawn(async move {
            if let Ok(Ok(size)) = timeout(wait_time, exchange).await {
                let _ = responses
                    .send(Response {
                        id,
                        timestamp: Instant::now(),
                        size,
                        ttl: None,
                    })
                    .await;
            }
        });
        Ok(started)
    }

    async fn recv(&self) -> io::Result<Response> {
        self.inner
            .responses_recv
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::BrokenPipe, "HTTP response channel closed")
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use http_body_util::Full;
    use hyper::server::conn::{http1 as server_http1, http2 as server_http2};
    use hyper::service::service_fn;
    use tokio::net::TcpListener;

    use crate::{Pinger, Protocol, SessionConfig, Target};

    const BODY: &str = "hello";

    struct TestServer {
        address: SocketAddr,
        connections: Arc<AtomicUsize>,
        /// `"<path?query> <host>"` of the most recent request.
        last_request: Arc<std::sync::Mutex<String>>,
    }

    /// Serves `BODY` to every request; HTTP/2 connections close after
    /// `requests_per_connection` requests to emulate a server request limit.
    async fn spawn_server(version: HttpVersion, requests_per_connection: usize) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let last_request = Arc::new(std::sync::Mutex::new(String::new()));
        let server = TestServer {
            address,
            connections: connections.clone(),
            last_request: last_request.clone(),
        };

        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                connections.fetch_add(1, Ordering::SeqCst);
                let last_request = last_request.clone();
                let served = Arc::new(AtomicUsize::new(0));
                let limit_reached = Arc::new(tokio::sync::Notify::new());
                let service = {
                    let limit_reached = limit_reached.clone();
                    service_fn(move |request: hyper::Request<Incoming>| {
                        // HTTP/2 carries the host in :authority instead of Host.
                        let host = request
                            .headers()
                            .get(HOST)
                            .map(|v| v.to_str().unwrap().to_owned())
                            .or_else(|| request.uri().authority().map(|a| a.to_string()))
                            .unwrap_or_default();
                        let path = request.uri().path_and_query().unwrap().as_str();
                        *last_request.lock().unwrap() = format!("{path} {host}");
                        if served.fetch_add(1, Ordering::SeqCst) + 1 == requests_per_connection {
                            limit_reached.notify_one();
                        }
                        async {
                            Ok::<_, Infallible>(hyper::Response::new(Full::new(Bytes::from(BODY))))
                        }
                    })
                };
                let io = TokioIo::new(stream);
                tokio::spawn(async move {
                    match version {
                        HttpVersion::Http1 => {
                            let _ = server_http1::Builder::new()
                                .serve_connection(io, service)
                                .await;
                        }
                        HttpVersion::Http2 => {
                            let connection = server_http2::Builder::new(TokioExecutor::new())
                                .serve_connection(io, service);
                            tokio::pin!(connection);
                            tokio::select! {
                                _ = connection.as_mut() => {}
                                _ = limit_reached.notified() => {
                                    connection.as_mut().graceful_shutdown();
                                    let _ = connection.await;
                                }
                            }
                        }
                    }
                });
            }
        });
        server
    }

    async fn connect(
        version: HttpVersion,
        target: &str,
        server: &TestServer,
    ) -> HttpClientTransport {
        HttpClientTransport::connect(
            version,
            "0.0.0.0:0".parse().unwrap(),
            server.address,
            None,
            target.parse().unwrap(),
            Duration::from_secs(1),
        )
        .await
        .unwrap()
    }

    fn request(id: u64) -> Request {
        Request {
            id,
            request_size: None,
            response_size: None,
        }
    }

    async fn exchange(transport: &HttpClientTransport, id: u64) -> (Instant, Response) {
        let sent = transport.send(&request(id)).await.unwrap();
        let response = timeout(Duration::from_secs(1), transport.recv())
            .await
            .unwrap()
            .unwrap();
        (sent, response)
    }

    #[tokio::test]
    async fn http1_uses_new_connection_per_request() {
        let server = spawn_server(HttpVersion::Http1, usize::MAX).await;
        let target = format!("http://localhost:{}/status?x=1", server.address.port());
        let transport = connect(HttpVersion::Http1, &target, &server).await;

        for id in 0..2 {
            let (sent, response) = exchange(&transport, id).await;
            assert_eq!(response.id, id);
            assert!(response.timestamp >= sent);
            assert!(response.size > BODY.len());
            assert_eq!(response.ttl, None);
        }

        // One warm-up connection plus one per request.
        assert_eq!(server.connections.load(Ordering::SeqCst), 3);
        assert_eq!(
            *server.last_request.lock().unwrap(),
            format!("/status?x=1 localhost:{}", server.address.port())
        );
    }

    #[tokio::test]
    async fn http2_reuses_one_connection() {
        let server = spawn_server(HttpVersion::Http2, usize::MAX).await;
        let target = format!("http://localhost:{}/h2", server.address.port());
        let transport = connect(HttpVersion::Http2, &target, &server).await;

        for id in 0..3 {
            let (_, response) = exchange(&transport, id).await;
            assert_eq!(response.id, id);
            assert!(response.size > BODY.len());
        }

        assert_eq!(server.connections.load(Ordering::SeqCst), 1);
        assert_eq!(
            *server.last_request.lock().unwrap(),
            format!("/h2 localhost:{}", server.address.port())
        );
    }

    #[tokio::test]
    async fn http2_reconnects_after_server_closes_connection() {
        // The warm-up request and request 0 share the first connection.
        let server = spawn_server(HttpVersion::Http2, 2).await;
        let target = format!("http://127.0.0.1:{}/", server.address.port());
        let transport = connect(HttpVersion::Http2, &target, &server).await;

        exchange(&transport, 0).await;
        // Let the client observe GOAWAY and the connection close.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (_, response) = exchange(&transport, 1).await;

        assert_eq!(response.id, 1);
        assert_eq!(server.connections.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn http2_connect_fails_without_server_http2_support() {
        let server = spawn_server(HttpVersion::Http1, usize::MAX).await;

        let result = HttpClientTransport::connect(
            HttpVersion::Http2,
            "0.0.0.0:0".parse().unwrap(),
            server.address,
            None,
            HttpTarget::from_socket_addr(server.address),
            Duration::from_secs(1),
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn connect_fails_for_closed_port() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = listener.local_addr().unwrap();
        drop(listener);

        for version in [HttpVersion::Http1, HttpVersion::Http2] {
            let error = HttpClientTransport::connect(
                version,
                "0.0.0.0:0".parse().unwrap(),
                remote,
                None,
                HttpTarget::from_socket_addr(remote),
                Duration::from_secs(1),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        }
    }

    #[tokio::test]
    async fn ping_session_reports_http_replies() {
        for (protocol, version) in [
            (Protocol::Http1, HttpVersion::Http1),
            (Protocol::Http2, HttpVersion::Http2),
        ] {
            let server = spawn_server(version, usize::MAX).await;
            let report = Pinger::new(Target::Http {
                version,
                remote: server.address,
                url: HttpTarget::from_socket_addr(server.address),
            })
            .session(
                SessionConfig::default()
                    .count(3)
                    .interval(Duration::from_millis(10)),
            )
            .run()
            .await
            .unwrap();

            assert_eq!(report.sent(), 3, "{protocol}");
            assert_eq!(report.received(), 3, "{protocol}");
        }
    }
}
