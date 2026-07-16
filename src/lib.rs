pub mod echo_codec;
pub mod pinger;
pub mod protocol;
pub mod statistics;
mod tos;
pub mod transport;

pub use pinger::{
    Echo, Entry, GeneratorConfig, PING_HDR_LEN, PacketSize, Request, Response, SendMode, StatEntry,
    generator,
};
pub use protocol::Protocol;
pub use statistics::{RttSequence, statista, statista_with_collector};
pub use transport::async_icmp::IcmpClientTransport;
pub use transport::async_tcp::TcpClientTransport;
pub use transport::async_udp::UdpClientTransport;
pub use transport::{Transport, receiver, transmitter};

use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const CHANNEL_CAP: usize = 1024;

struct SessionChannels {
    generator_to_transmitter: mpsc::Sender<pinger::Request>,
    transmitter_input: mpsc::Receiver<pinger::Request>,
    transport_to_statista: mpsc::Sender<pinger::StatEntry>,
    statista_input: mpsc::Receiver<pinger::StatEntry>,
}

impl SessionChannels {
    fn new() -> Self {
        let (generator_to_transmitter, transmitter_input) = mpsc::channel(CHANNEL_CAP);
        let (transport_to_statista, statista_input) = mpsc::channel(CHANNEL_CAP);

        Self {
            generator_to_transmitter,
            transmitter_input,
            transport_to_statista,
            statista_input,
        }
    }
}

pub fn has_port(addr: &str) -> bool {
    if addr.starts_with('[') {
        let after_bracket = addr.split(']').nth(1).unwrap_or("");
        after_bracket.starts_with(':')
    } else {
        if addr.matches(':').count() > 1 {
            return false;
        }
        let last_colon = addr.rfind(':');
        match last_colon {
            Some(i) => {
                let after = &addr[i + 1..];
                !after.is_empty() && after.chars().all(|c| c.is_ascii_digit())
            }
            None => false,
        }
    }
}

pub fn ensure_port(addr: &str, protocol: Protocol) -> io::Result<String> {
    if has_port(addr) {
        return Ok(addr.to_string());
    }
    if protocol == Protocol::Icmp {
        if addr.contains(':') && !addr.starts_with('[') {
            return Ok(format!("[{addr}]:0"));
        }
        return Ok(format!("{addr}:0"));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{protocol} requires a port (e.g. {addr}:PORT)"),
    ))
}

/// A single RTT measurement result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingResult {
    pub seq: u64,
    pub rtt: Duration,
    pub size: usize,
    pub ttl: Option<u8>,
}

/// Live event produced by a running ping session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PingEvent {
    Reply(PingResult),
    Timeout { seq: u64 },
    ReorderOrLoss { seq: u64 },
}

/// Summary report from a completed ping session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingReport {
    pub rtts: Vec<Duration>,
    pub sent: u64,
    pub received: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrafficClass(u8);

impl TrafficClass {
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl PingReport {
    pub fn loss_pct(&self) -> f64 {
        statistics::loss_pct(self.sent, self.received)
    }

    pub fn min(&self) -> Option<Duration> {
        self.rtts.iter().min().copied()
    }

    pub fn max(&self) -> Option<Duration> {
        self.rtts.iter().max().copied()
    }

    pub fn mean(&self) -> Option<Duration> {
        statistics::mean(&self.rtts)
    }

    pub fn median(&self) -> Option<Duration> {
        statistics::median(&self.rtts)
    }

    pub fn std_dev(&self) -> Option<Duration> {
        statistics::std_deviation(&self.rtts)
    }
}

/// A running ping session that yields live events and eventually a summary report.
pub struct PingSession {
    events: mpsc::Receiver<PingEvent>,
    report: Option<JoinHandle<io::Result<PingReport>>>,
}

impl PingSession {
    fn new(events: mpsc::Receiver<PingEvent>, report: JoinHandle<io::Result<PingReport>>) -> Self {
        Self {
            events,
            report: Some(report),
        }
    }

    pub async fn next(&mut self) -> Option<PingEvent> {
        self.events.recv().await
    }

    pub async fn report(mut self) -> io::Result<PingReport> {
        self.events.close();
        let report = self
            .report
            .take()
            .ok_or_else(|| io::Error::other("ping session report already taken"))?;

        wait_ping_report(report).await
    }
}

/// High-level builder for quick ping sessions.
///
/// # Example
///
/// ```no_run
/// use rup::{Pinger, Protocol};
///
/// # async fn example() -> std::io::Result<()> {
/// let report = Pinger::new("127.0.0.1:5000".parse().unwrap(), Protocol::Udp)
///     .count(5)
///     .interval(1000)
///     .run()
///     .await?;
/// println!("min/avg/max = {:?}/{:?}/{:?}", report.min(), report.mean(), report.max());
/// # Ok(())
/// # }
/// ```
pub struct Pinger {
    config: PingConfig,
}

impl Pinger {
    pub fn new(remote: SocketAddr, protocol: Protocol) -> Self {
        Pinger {
            config: PingConfig::new(remote, protocol),
        }
    }

    pub fn count(mut self, n: u64) -> Self {
        self.config.ping_number = Some(n);
        self
    }

    pub fn interval(mut self, ms: u64) -> Self {
        self.config.interval = Duration::from_millis(ms);
        self
    }

    pub fn adaptive(mut self) -> Self {
        self.config.adaptive = true;
        self
    }

    pub fn wait_time(mut self, ms: u64) -> Self {
        self.config.wait_time = Duration::from_millis(ms);
        self
    }

    pub fn request_size(mut self, size: PacketSize) -> Self {
        self.config.request_size = Some(size);
        self
    }

    pub fn response_size(mut self, size: PacketSize) -> Self {
        self.config.response_size = Some(size);
        self
    }

    pub fn tos(mut self, tos: u8) -> Self {
        self.config.tos = Some(TrafficClass::new(tos));
        self
    }

    pub fn local(mut self, addr: SocketAddr) -> Self {
        self.config.local = addr;
        self
    }

    pub fn run_time(mut self, dur: Duration) -> Self {
        self.config.run_time = Some(dur);
        self
    }

    pub async fn run(self) -> io::Result<PingReport> {
        run_ping_session(self.config).await
    }
}

#[derive(Clone, Debug)]
pub struct PingConfig {
    pub remote: SocketAddr,
    pub local: SocketAddr,
    pub protocol: Protocol,
    pub interval: Duration,
    pub adaptive: bool,
    pub wait_time: Duration,
    pub request_size: Option<PacketSize>,
    pub response_size: Option<PacketSize>,
    pub tos: Option<TrafficClass>,
    pub ping_number: Option<u64>,
    pub run_time: Option<Duration>,
}

impl PingConfig {
    pub fn new(remote: SocketAddr, protocol: Protocol) -> Self {
        Self {
            remote,
            local: "0.0.0.0:0".parse().unwrap(),
            protocol,
            interval: Duration::from_millis(1000),
            adaptive: false,
            wait_time: Duration::from_millis(1000),
            request_size: None,
            response_size: None,
            tos: None,
            ping_number: None,
            run_time: None,
        }
    }
}

pub async fn run_ping_session(config: PingConfig) -> io::Result<PingReport> {
    let report = spawn_ping_session(config, None).await?;
    wait_ping_report(report).await
}

pub async fn start_ping_session(config: PingConfig) -> io::Result<PingSession> {
    let (events_send, events_recv) = mpsc::channel(CHANNEL_CAP);
    let report = spawn_ping_session(config, Some(events_send)).await?;
    Ok(PingSession::new(events_recv, report))
}

async fn wait_ping_report(report: JoinHandle<io::Result<PingReport>>) -> io::Result<PingReport> {
    report
        .await
        .map_err(|e| io::Error::other(format!("ping session task failed: {e}")))?
}

async fn spawn_ping_session(
    config: PingConfig,
    events: Option<mpsc::Sender<PingEvent>>,
) -> io::Result<JoinHandle<io::Result<PingReport>>> {
    let remote_addr = config.remote;

    match config.protocol {
        Protocol::Udp => {
            let transport =
                UdpClientTransport::new_with_tos(config.local, remote_addr, config.tos).await?;
            Ok(spawn_ping_with_transport(transport, config, events))
        }
        Protocol::Tcp => {
            let transport = TcpClientTransport::connect_or_probe(
                config.local,
                remote_addr,
                config.tos,
                config.wait_time,
            )
            .await?;
            Ok(spawn_ping_with_transport(transport, config, events))
        }
        Protocol::Icmp => {
            let transport =
                IcmpClientTransport::new_with_tos(config.local, remote_addr, config.tos).await?;
            Ok(spawn_ping_with_transport(transport, config, events))
        }
    }
}

fn spawn_ping_with_transport<T>(
    transport: T,
    config: PingConfig,
    events: Option<mpsc::Sender<PingEvent>>,
) -> JoinHandle<io::Result<PingReport>>
where
    T: Transport + Clone + Send + 'static,
{
    tokio::spawn(run_ping_with_transport(transport, config, events))
}

#[cfg(test)]
fn start_ping_with_transport<T>(transport: T, config: PingConfig) -> PingSession
where
    T: Transport + Clone + Send + 'static,
{
    let (events_send, events_recv) = mpsc::channel(CHANNEL_CAP);
    PingSession::new(
        events_recv,
        spawn_ping_with_transport(transport, config, Some(events_send)),
    )
}

async fn run_ping_with_transport<T>(
    transport: T,
    config: PingConfig,
    events: Option<mpsc::Sender<PingEvent>>,
) -> io::Result<PingReport>
where
    T: Transport + Clone + Send + 'static,
{
    let channels = SessionChannels::new();

    let (send_mode, txtr_gen) = if config.adaptive {
        let (txtr_gen_send, txtr_gen_recv) = mpsc::channel(CHANNEL_CAP);
        (SendMode::Adaptive(txtr_gen_recv), Some(txtr_gen_send))
    } else {
        (SendMode::Interval(config.interval), None)
    };

    let (sends_done_send, sends_done_recv) = mpsc::channel(1);
    let (tx_handle, rx_handle) = spawn_pinger_tasks(
        transport,
        channels.transmitter_input,
        channels.transport_to_statista,
        sends_done_send,
    );

    let generator = tokio::spawn(pinger::generator(
        channels.generator_to_transmitter,
        pinger::GeneratorConfig {
            send_mode,
            ping_number: config.ping_number,
            run_time: config.run_time,
            request_size: config.request_size,
            response_size: config.response_size,
        },
    ));

    let statista = if let Some(events) = events {
        tokio::spawn(statistics::statista_with_events_and_send_done(
            channels.statista_input,
            sends_done_recv,
            txtr_gen,
            config.wait_time,
            events,
        ))
    } else {
        tokio::spawn(statistics::statista_report_with_send_done(
            channels.statista_input,
            sends_done_recv,
            txtr_gen,
            config.wait_time,
        ))
    };

    let _ = generator.await;
    let _ = tx_handle.await;
    let stat_report = statista
        .await
        .map_err(|e| io::Error::other(format!("statista task failed: {e}")))?;

    rx_handle.abort();

    Ok(stat_report)
}

pub async fn run_server(protocol: Protocol, local: SocketAddr) -> io::Result<()> {
    run_server_until(protocol, local, std::future::pending()).await
}

pub async fn run_server_until(
    protocol: Protocol,
    local: SocketAddr,
    shutdown: impl std::future::Future<Output = ()>,
) -> io::Result<()> {
    match protocol {
        Protocol::Tcp => transport::async_tcp::server_transport_until(local, shutdown).await?,
        Protocol::Udp => transport::async_udp::server_transport_until(local, shutdown).await?,
        Protocol::Icmp => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "there is no server for ICMP",
            ));
        }
    }
    Ok(())
}

fn spawn_pinger_tasks<T: Transport + Clone + Send + 'static>(
    transport: T,
    gen_txtr_recv: mpsc::Receiver<pinger::Request>,
    txtr_stat_send: mpsc::Sender<pinger::StatEntry>,
    sends_done: mpsc::Sender<()>,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let t2 = transport.clone();
    let txtr_stat_send_for_tx = txtr_stat_send.clone();
    let tx = tokio::spawn(async move {
        transmitter(t2, gen_txtr_recv, txtr_stat_send_for_tx).await;
        let _ = sends_done.send(()).await;
    });
    let rx = tokio::spawn(receiver(transport, txtr_stat_send));
    (tx, rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::Mutex;

    #[derive(Clone)]
    struct ScriptedTransport {
        replies: Arc<Mutex<VecDeque<u64>>>,
        pending: Arc<Mutex<VecDeque<u64>>>,
    }

    #[derive(Clone)]
    struct OrderedScriptedTransport {
        replies: Arc<Mutex<VecDeque<u64>>>,
        pending: Arc<Mutex<VecDeque<u64>>>,
    }

    #[derive(Clone)]
    struct DelayedScriptedTransport {
        replies: Arc<Mutex<VecDeque<u64>>>,
        pending: Arc<Mutex<VecDeque<u64>>>,
        delay: Duration,
    }

    impl ScriptedTransport {
        fn new(responses: impl IntoIterator<Item = u64>) -> Self {
            Self {
                replies: Arc::new(Mutex::new(responses.into_iter().collect())),
                pending: Arc::new(Mutex::new(VecDeque::new())),
            }
        }
    }

    impl OrderedScriptedTransport {
        fn new(responses: impl IntoIterator<Item = u64>) -> Self {
            Self {
                replies: Arc::new(Mutex::new(responses.into_iter().collect())),
                pending: Arc::new(Mutex::new(VecDeque::new())),
            }
        }
    }

    impl DelayedScriptedTransport {
        fn new(responses: impl IntoIterator<Item = u64>, delay: Duration) -> Self {
            Self {
                replies: Arc::new(Mutex::new(responses.into_iter().collect())),
                pending: Arc::new(Mutex::new(VecDeque::new())),
                delay,
            }
        }
    }

    impl Transport for ScriptedTransport {
        async fn send(&self, req: &Request) -> io::Result<Instant> {
            let mut replies = self.replies.lock().await;
            if replies.front() == Some(&req.id) {
                replies.pop_front();
                self.pending.lock().await.push_back(req.id);
            }
            Ok(Instant::now())
        }

        async fn recv(&self) -> io::Result<Response> {
            loop {
                if let Some(id) = self.pending.lock().await.pop_front() {
                    return Ok(Response {
                        id,
                        timestamp: Instant::now(),
                        size: 0,
                        ttl: None,
                    });
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    }

    impl Transport for OrderedScriptedTransport {
        async fn send(&self, req: &Request) -> io::Result<Instant> {
            let mut replies = self.replies.lock().await;
            if replies.front() == Some(&req.id) {
                replies.pop_front();
                let pending = self.pending.clone();
                let id = req.id;
                tokio::spawn(async move {
                    tokio::task::yield_now().await;
                    pending.lock().await.push_back(id);
                });
            }
            Ok(Instant::now())
        }

        async fn recv(&self) -> io::Result<Response> {
            loop {
                if let Some(id) = self.pending.lock().await.pop_front() {
                    return Ok(Response {
                        id,
                        timestamp: Instant::now(),
                        size: 0,
                        ttl: None,
                    });
                }
                tokio::task::yield_now().await;
            }
        }
    }

    impl Transport for DelayedScriptedTransport {
        async fn send(&self, req: &Request) -> io::Result<Instant> {
            let mut replies = self.replies.lock().await;
            if replies.front() == Some(&req.id) {
                replies.pop_front();
                let pending = self.pending.clone();
                let delay = self.delay;
                let id = req.id;
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    pending.lock().await.push_back(id);
                });
            }
            Ok(Instant::now())
        }

        async fn recv(&self) -> io::Result<Response> {
            loop {
                if let Some(id) = self.pending.lock().await.pop_front() {
                    return Ok(Response {
                        id,
                        timestamp: Instant::now(),
                        size: 0,
                        ttl: None,
                    });
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    }

    #[test]
    fn has_port_detects_v4_with_port() {
        assert!(has_port("127.0.0.1:5000"));
    }

    #[test]
    fn has_port_detects_v4_without_port() {
        assert!(!has_port("127.0.0.1"));
    }

    #[test]
    fn has_port_detects_v6_with_port() {
        assert!(has_port("[::1]:5000"));
    }

    #[test]
    fn has_port_detects_v6_without_port() {
        assert!(!has_port("[::1]"));
    }

    #[test]
    fn has_port_detects_hostname_with_port() {
        assert!(has_port("localhost:8080"));
    }

    #[test]
    fn has_port_detects_hostname_without_port() {
        assert!(!has_port("localhost"));
    }

    #[test]
    fn has_port_empty_after_colon() {
        assert!(!has_port("127.0.0.1:"));
    }

    #[test]
    fn has_port_non_numeric_after_colon() {
        assert!(!has_port("127.0.0.1:abc"));
    }

    #[test]
    fn has_port_multiple_colons_ipv6() {
        assert!(has_port("[2001:db8::1]:8080"));
    }

    #[test]
    fn has_port_multiple_colons_no_port() {
        assert!(!has_port("[2001:db8::1]"));
    }

    #[test]
    fn has_port_empty_string() {
        assert!(!has_port(""));
    }

    #[test]
    fn has_port_just_port_number() {
        assert!(has_port(":5000"));
    }

    #[test]
    fn ensure_port_keeps_existing_v4() {
        assert_eq!(
            ensure_port("10.0.0.1:9999", Protocol::Udp).unwrap(),
            "10.0.0.1:9999"
        );
    }

    #[test]
    fn ensure_port_keeps_existing_v6() {
        assert_eq!(
            ensure_port("[::1]:443", Protocol::Tcp).unwrap(),
            "[::1]:443"
        );
    }

    #[test]
    fn ensure_port_adds_zero_for_icmp_v4() {
        assert_eq!(
            ensure_port("192.168.1.1", Protocol::Icmp).unwrap(),
            "192.168.1.1:0"
        );
    }

    #[test]
    fn ensure_port_adds_zero_for_icmp_v6() {
        assert_eq!(ensure_port("[::1]", Protocol::Icmp).unwrap(), "[::1]:0");
    }

    #[test]
    fn ensure_port_brackets_unbracketed_icmp_v6() {
        assert_eq!(ensure_port("::1", Protocol::Icmp).unwrap(), "[::1]:0");
    }

    #[test]
    fn ensure_port_adds_zero_for_icmp_hostname() {
        assert_eq!(
            ensure_port("localhost", Protocol::Icmp).unwrap(),
            "localhost:0"
        );
    }

    #[test]
    fn ensure_port_errors_for_udp_without_port() {
        assert!(ensure_port("localhost", Protocol::Udp).is_err());
    }

    #[test]
    fn ping_report_empty() {
        let report = PingReport {
            rtts: vec![],
            sent: 0,
            received: 0,
        };
        assert_eq!(report.loss_pct(), 0.0);
        assert!(report.min().is_none());
        assert!(report.max().is_none());
        assert!(report.mean().is_none());
        assert!(report.median().is_none());
        assert!(report.std_dev().is_none());
    }

    #[test]
    fn ping_config_new_uses_resolved_socket_address() {
        let remote = "127.0.0.1:5000".parse().unwrap();
        let config = PingConfig::new(remote, Protocol::Udp);

        assert_eq!(config.remote, remote);
        assert_eq!(config.protocol, Protocol::Udp);
    }

    #[tokio::test]
    async fn run_ping_session_udp_loopback_report() {
        use tokio::net::UdpSocket;

        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let mut buf = vec![0; u16::MAX as usize];
            for _ in 0..3 {
                let (n, addr) = server_sock.recv_from(&mut buf).await.unwrap();
                server_sock.send_to(&buf[..n], addr).await.unwrap();
            }
        });

        let report = run_ping_session(PingConfig {
            remote: server_addr,
            local: "0.0.0.0:0".parse().unwrap(),
            protocol: Protocol::Udp,
            interval: Duration::from_millis(1),
            adaptive: false,
            wait_time: Duration::from_millis(100),
            request_size: None,
            response_size: None,
            tos: None,
            ping_number: Some(3),
            run_time: None,
        })
        .await
        .unwrap();

        assert_eq!(report.sent, 3);
        assert_eq!(report.received, 3);
        assert_eq!(report.rtts.len(), 3);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn run_ping_session_udp_no_response_reports_loss() {
        use tokio::net::UdpSocket;

        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let _ = server_sock.recv_from(&mut buf).await;
        });

        let report = run_ping_session(PingConfig {
            remote: server_addr,
            local: "0.0.0.0:0".parse().unwrap(),
            protocol: Protocol::Udp,
            interval: Duration::from_millis(1),
            adaptive: false,
            wait_time: Duration::from_millis(5),
            request_size: None,
            response_size: None,
            tos: None,
            ping_number: Some(1),
            run_time: None,
        })
        .await
        .unwrap();

        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 0);
        assert_eq!(report.loss_pct(), 100.0);
        server.await.unwrap();
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn run_ping_session_udp_closed_port_uses_icmp_error() {
        use tokio::net::UdpSocket;

        let unused_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let unused_addr = unused_socket.local_addr().unwrap();
        drop(unused_socket);

        let report = run_ping_session(PingConfig {
            remote: unused_addr,
            local: "0.0.0.0:0".parse().unwrap(),
            protocol: Protocol::Udp,
            interval: Duration::from_millis(1),
            adaptive: false,
            wait_time: Duration::from_millis(100),
            request_size: None,
            response_size: None,
            tos: None,
            ping_number: Some(1),
            run_time: None,
        })
        .await
        .unwrap();

        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 1);
        assert_eq!(report.loss_pct(), 0.0);
    }

    #[tokio::test]
    async fn run_ping_session_tcp_loopback_report() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server_addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            for _ in 0..2 {
                let mut hdr = [0; PING_HDR_LEN];
                stream.read_exact(&mut hdr).await.unwrap();
                let echo = echo_codec::decode_header(&hdr).unwrap();
                if echo.len as usize > PING_HDR_LEN {
                    let mut extra = vec![0; echo.len as usize - PING_HDR_LEN];
                    stream.read_exact(&mut extra).await.unwrap();
                }
                stream.write_all(&hdr).await.unwrap();
            }
        });

        let report = run_ping_session(PingConfig {
            remote: server_addr,
            local: "0.0.0.0:0".parse().unwrap(),
            protocol: Protocol::Tcp,
            interval: Duration::from_millis(1),
            adaptive: false,
            wait_time: Duration::from_millis(100),
            request_size: None,
            response_size: None,
            tos: None,
            ping_number: Some(2),
            run_time: None,
        })
        .await
        .unwrap();

        assert_eq!(report.sent, 2);
        assert_eq!(report.received, 2);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn run_ping_session_tcp_closed_port_advances_adaptive_mode() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = listener.local_addr().unwrap();
        drop(listener);

        let report = run_ping_session(PingConfig {
            remote,
            local: "0.0.0.0:0".parse().unwrap(),
            protocol: Protocol::Tcp,
            interval: Duration::from_millis(1),
            adaptive: true,
            wait_time: Duration::from_millis(100),
            request_size: None,
            response_size: None,
            tos: None,
            ping_number: Some(2),
            run_time: None,
        })
        .await
        .unwrap();

        assert_eq!(report.sent, 2);
        assert_eq!(report.received, 2);
        assert_eq!(report.loss_pct(), 0.0);
    }

    #[tokio::test]
    async fn run_ping_with_transport_reports_success() {
        let report = run_ping_with_transport(
            ScriptedTransport::new([0, 1, 2]),
            PingConfig {
                remote: "127.0.0.1:0".parse().unwrap(),
                local: "0.0.0.0:0".parse().unwrap(),
                protocol: Protocol::Udp,
                interval: Duration::from_millis(1),
                adaptive: false,
                wait_time: Duration::from_millis(100),
                request_size: None,
                response_size: None,
                tos: None,
                ping_number: Some(3),
                run_time: None,
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.sent, 3);
        assert_eq!(report.received, 3);
        assert_eq!(report.rtts.len(), 3);
    }

    #[tokio::test]
    async fn run_ping_with_transport_handles_many_responses() {
        let count = 2050;
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            run_ping_with_transport(
                OrderedScriptedTransport::new(0..count),
                PingConfig {
                    remote: "127.0.0.1:0".parse().unwrap(),
                    local: "0.0.0.0:0".parse().unwrap(),
                    protocol: Protocol::Udp,
                    interval: Duration::from_millis(1000),
                    adaptive: true,
                    wait_time: Duration::from_millis(100),
                    request_size: None,
                    response_size: None,
                    tos: None,
                    ping_number: Some(count),
                    run_time: None,
                },
                None,
            ),
        )
        .await
        .expect("ping session should not stall after channel capacity")
        .unwrap();

        assert_eq!(report.sent, count);
        assert_eq!(report.received, count);
        assert_eq!(report.rtts.len() as u64, count);
    }

    #[tokio::test]
    async fn run_ping_with_transport_reports_loss() {
        let report = run_ping_with_transport(
            ScriptedTransport::new([]),
            PingConfig {
                remote: "127.0.0.1:0".parse().unwrap(),
                local: "0.0.0.0:0".parse().unwrap(),
                protocol: Protocol::Udp,
                interval: Duration::from_millis(1),
                adaptive: false,
                wait_time: Duration::from_millis(2),
                request_size: None,
                response_size: None,
                tos: None,
                ping_number: Some(2),
                run_time: None,
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.sent, 2);
        assert_eq!(report.received, 0);
        assert_eq!(report.loss_pct(), 100.0);
    }

    #[tokio::test]
    async fn run_ping_with_transport_waits_for_delayed_final_response() {
        let report = run_ping_with_transport(
            DelayedScriptedTransport::new([0], Duration::from_millis(10)),
            PingConfig {
                remote: "127.0.0.1:0".parse().unwrap(),
                local: "0.0.0.0:0".parse().unwrap(),
                protocol: Protocol::Udp,
                interval: Duration::from_millis(1),
                adaptive: false,
                wait_time: Duration::from_millis(100),
                request_size: None,
                response_size: None,
                tos: None,
                ping_number: Some(1),
                run_time: None,
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 1);
        assert_eq!(report.rtts.len(), 1);
    }

    #[tokio::test]
    async fn run_ping_with_transport_waits_for_final_timeout() {
        let wait_time = Duration::from_millis(20);
        let started = Instant::now();
        let report = run_ping_with_transport(
            ScriptedTransport::new([]),
            PingConfig {
                remote: "127.0.0.1:0".parse().unwrap(),
                local: "0.0.0.0:0".parse().unwrap(),
                protocol: Protocol::Udp,
                interval: Duration::from_millis(1),
                adaptive: false,
                wait_time,
                request_size: None,
                response_size: None,
                tos: None,
                ping_number: Some(1),
                run_time: None,
            },
            None,
        )
        .await
        .unwrap();

        assert!(started.elapsed() >= wait_time);
        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 0);
    }

    #[tokio::test]
    async fn run_ping_with_transport_waits_for_ten_sent_five_received_case() {
        let wait_time = Duration::from_millis(120);
        let started = Instant::now();
        let report = run_ping_with_transport(
            ScriptedTransport::new(0..5),
            PingConfig {
                remote: "127.0.0.1:0".parse().unwrap(),
                local: "0.0.0.0:0".parse().unwrap(),
                protocol: Protocol::Udp,
                interval: Duration::from_millis(10),
                adaptive: false,
                wait_time,
                request_size: None,
                response_size: None,
                tos: None,
                ping_number: Some(10),
                run_time: None,
            },
            None,
        )
        .await
        .unwrap();

        assert!(started.elapsed() >= wait_time);
        assert_eq!(report.sent, 10);
        assert_eq!(report.received, 5);
        assert_eq!(report.loss_pct(), 50.0);
    }

    #[tokio::test]
    async fn run_ping_with_transport_supports_adaptive_mode() {
        let report = run_ping_with_transport(
            ScriptedTransport::new([0, 1, 2]),
            PingConfig {
                remote: "127.0.0.1:0".parse().unwrap(),
                local: "0.0.0.0:0".parse().unwrap(),
                protocol: Protocol::Udp,
                interval: Duration::from_millis(1000),
                adaptive: true,
                wait_time: Duration::from_millis(100),
                request_size: None,
                response_size: None,
                tos: None,
                ping_number: Some(3),
                run_time: None,
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(report.sent, 3);
        assert_eq!(report.received, 3);
    }

    #[tokio::test]
    async fn ping_session_yields_reply_events_then_report() {
        let mut session = start_ping_with_transport(
            ScriptedTransport::new([0, 1]),
            PingConfig {
                remote: "127.0.0.1:0".parse().unwrap(),
                local: "0.0.0.0:0".parse().unwrap(),
                protocol: Protocol::Udp,
                interval: Duration::from_millis(1),
                adaptive: false,
                wait_time: Duration::from_millis(100),
                request_size: None,
                response_size: None,
                tos: None,
                ping_number: Some(2),
                run_time: None,
            },
        );

        let mut events = Vec::new();
        while let Some(event) = session.next().await {
            events.push(event);
        }
        let report = session.report().await.unwrap();

        assert_eq!(report.sent, 2);
        assert_eq!(report.received, 2);
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            PingEvent::Reply(PingResult { seq: 0, .. })
        ));
        assert!(matches!(
            events[1],
            PingEvent::Reply(PingResult { seq: 1, .. })
        ));
    }

    #[tokio::test]
    async fn ping_session_reports_timeouts_for_ten_sent_five_received_case() {
        let mut session = start_ping_with_transport(
            ScriptedTransport::new(0..5),
            PingConfig {
                remote: "127.0.0.1:0".parse().unwrap(),
                local: "0.0.0.0:0".parse().unwrap(),
                protocol: Protocol::Udp,
                interval: Duration::from_millis(10),
                adaptive: false,
                wait_time: Duration::from_millis(120),
                request_size: None,
                response_size: None,
                tos: None,
                ping_number: Some(10),
                run_time: None,
            },
        );

        let mut replies = Vec::new();
        let mut timeouts = Vec::new();
        while let Some(event) = session.next().await {
            match event {
                PingEvent::Reply(result) => replies.push(result.seq),
                PingEvent::Timeout { seq } => timeouts.push(seq),
                PingEvent::ReorderOrLoss { seq } => panic!("unexpected reorder/loss for seq {seq}"),
            }
        }
        replies.sort_unstable();
        timeouts.sort_unstable();

        let report = session.report().await.unwrap();

        assert_eq!(replies, vec![0, 1, 2, 3, 4]);
        assert_eq!(timeouts, vec![5, 6, 7, 8, 9]);
        assert_eq!(report.sent, 10);
        assert_eq!(report.received, 5);
        assert_eq!(report.loss_pct(), 50.0);
    }

    #[tokio::test]
    async fn run_server_rejects_icmp() {
        let err = run_server_until(
            Protocol::Icmp,
            "127.0.0.1:0".parse().unwrap(),
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn run_server_propagates_tcp_bind_error() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let err = run_server_until(Protocol::Tcp, addr, std::future::pending())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
    }

    #[tokio::test]
    async fn run_server_stops_on_injected_shutdown() {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(run_server_until(
            Protocol::Udp,
            "127.0.0.1:0".parse().unwrap(),
            async {
                let _ = shutdown_rx.await;
            },
        ));

        shutdown_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[test]
    fn ping_report_statistics() {
        let report = PingReport {
            rtts: vec![
                Duration::from_micros(100),
                Duration::from_micros(200),
                Duration::from_micros(300),
            ],
            sent: 3,
            received: 3,
        };
        assert!((report.loss_pct() - 0.0).abs() < f64::EPSILON);
        assert_eq!(report.min(), Some(Duration::from_micros(100)));
        assert_eq!(report.max(), Some(Duration::from_micros(300)));
        assert_eq!(report.mean(), Some(Duration::from_micros(200)));
    }
}
