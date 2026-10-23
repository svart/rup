use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tracing::Instrument;

use crate::http_target::{HttpTarget, HttpVersion};
use crate::pinger::{self, PacketSize, SendMode};
use crate::protocol::Protocol;
use crate::statistics;
use crate::transport::async_http::HttpClientTransport;
use crate::transport::async_icmp::IcmpClientTransport;
use crate::transport::async_tcp::TcpClientTransport;
use crate::transport::async_udp::UdpClientTransport;
use crate::transport::{self, Transport, receiver, transmitter};

const CHANNEL_CAP: usize = 1024;

struct SessionChannels {
    generator_to_transmitter: mpsc::Sender<pinger::Request>,
    transmitter_input: mpsc::Receiver<pinger::Request>,
    transport_to_statista: mpsc::Sender<pinger::StatEntry>,
    statista_input: mpsc::Receiver<pinger::StatEntry>,
}

enum SessionTask {
    Generator,
    Transmitter(io::Result<()>),
    Receiver(io::Result<()>),
    Statistics(PingReport),
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

/// Returns `true` when `addr` already contains an explicit port.
///
/// Recognizes bracketed IPv6 addresses and bare `host:port` strings.
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

/// Returns `addr` as `host:port` for DNS resolution.
///
/// For HTTP protocols `addr` is a URL and the port defaults from its scheme.
pub fn ensure_port(addr: &str, protocol: Protocol) -> io::Result<String> {
    if protocol.http_version().is_some() {
        return addr
            .parse::<HttpTarget>()
            .map(|target| target.host_port())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e));
    }
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
    /// Sequence number of the request that produced this reply.
    pub seq: u64,
    /// Measured round-trip time.
    pub rtt: Duration,
    /// When the transport received the reply; the RTT ends at this instant.
    pub received_at: Instant,
    /// Reply payload size in bytes; zero for terminal network responses.
    pub size: usize,
    /// Observed TTL when the transport can read it.
    pub ttl: Option<u8>,
}

/// Live event produced by a running ping session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PingEvent {
    /// A reply matched a request and produced an RTT sample.
    Reply(PingResult),
    /// The request reached its timeout without a reply.
    Timeout {
        /// Sequence number of the timed-out request.
        seq: u64,
    },
    /// A later reply arrived before this request, so it is treated as lost or reordered.
    ReorderOrLoss {
        /// Sequence number of the skipped request.
        seq: u64,
    },
}

/// Summary report from a completed ping session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingReport {
    pub(crate) rtts: Vec<Duration>,
    pub(crate) sent: u64,
    pub(crate) received: u64,
}

/// Error returned when a report would contain more samples than sent packets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PingReportError {
    sent: u64,
    received: u64,
}

impl fmt::Display for PingReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "received count {} exceeds sent count {}",
            self.received, self.sent
        )
    }
}

impl std::error::Error for PingReportError {}

/// Full IP TOS / IPv6 traffic class byte applied to outgoing packets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrafficClass(u8);

impl TrafficClass {
    /// Wraps a raw TOS / traffic class byte.
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Returns the raw byte.
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl PingReport {
    /// Builds a report from a sent count and the collected RTT samples.
    ///
    /// Returns [`PingReportError`] when there are more samples than sent packets.
    pub fn from_rtts(sent: u64, rtts: Vec<Duration>) -> Result<Self, PingReportError> {
        let received = rtts.len() as u64;
        if received > sent {
            return Err(PingReportError { sent, received });
        }
        Ok(Self {
            received,
            sent,
            rtts,
        })
    }

    /// Number of requests sent.
    pub fn sent(&self) -> u64 {
        self.sent
    }

    /// Number of RTT samples collected.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// RTT samples in completion order.
    pub fn rtts(&self) -> &[Duration] {
        &self.rtts
    }

    /// Packet loss as a percentage (`0.0..=100.0`).
    pub fn loss_pct(&self) -> f64 {
        statistics::loss_pct(self.sent, self.received)
    }

    /// Smallest RTT, or `None` without samples.
    pub fn min(&self) -> Option<Duration> {
        self.rtts.iter().min().copied()
    }

    /// Largest RTT, or `None` without samples.
    pub fn max(&self) -> Option<Duration> {
        self.rtts.iter().max().copied()
    }

    /// Arithmetic mean RTT, or `None` without samples.
    pub fn mean(&self) -> Option<Duration> {
        statistics::mean(&self.rtts)
    }

    /// Median RTT, or `None` without samples.
    pub fn median(&self) -> Option<Duration> {
        statistics::median(&self.rtts)
    }

    /// Population standard deviation of the RTT samples.
    pub fn std_dev(&self) -> Option<Duration> {
        statistics::std_deviation(&self.rtts)
    }
}

/// A running ping session that yields live events and eventually a summary report.
pub struct PingSession {
    events: mpsc::Receiver<PingEvent>,
    report: Option<JoinHandle<io::Result<PingReport>>>,
    stop: Option<oneshot::Sender<()>>,
}

impl PingSession {
    fn new(
        events: mpsc::Receiver<PingEvent>,
        report: JoinHandle<io::Result<PingReport>>,
        stop: oneshot::Sender<()>,
    ) -> Self {
        Self {
            events,
            report: Some(report),
            stop: Some(stop),
        }
    }

    /// Waits for the next live event, or `None` once the session has ended.
    pub async fn next(&mut self) -> Option<PingEvent> {
        self.events.recv().await
    }

    /// Stops generation of new requests while allowing pending results to settle.
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    /// Stops the session, waits for outstanding results, and returns the report.
    ///
    /// Events not yet read with [`PingSession::next`] are discarded.
    pub async fn report(mut self) -> io::Result<PingReport> {
        self.stop();
        self.events.close();
        let report = self
            .report
            .take()
            .ok_or_else(|| io::Error::other("ping session report already taken"))?;

        wait_ping_report(report).await
    }
}

/// Destination of a ping session, with the data that its transport needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    /// UDP echo requests to `addr`.
    Udp(SocketAddr),
    /// TCP echo requests, or connect probes, to `addr`.
    Tcp(SocketAddr),
    /// ICMP echo requests to `addr`. The port is ignored.
    Icmp(SocketAddr),
    /// `GET` requests for `url`; `remote` is the resolved address of the URL host.
    Http {
        /// HTTP version to speak.
        version: HttpVersion,
        /// Resolved socket address of the URL host.
        remote: SocketAddr,
        /// Parsed request URL.
        url: HttpTarget,
    },
}

impl Target {
    fn remote(&self) -> SocketAddr {
        match self {
            Target::Udp(remote) | Target::Tcp(remote) | Target::Icmp(remote) => *remote,
            Target::Http { remote, .. } => *remote,
        }
    }
}

/// Transport-independent session settings: request schedule, sizes, and reply timeout.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    interval: Duration,
    adaptive: bool,
    timeout: Duration,
    request_size: Option<PacketSize>,
    response_size: Option<PacketSize>,
    count: Option<u64>,
    run_time: Option<Duration>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(1000),
            adaptive: false,
            timeout: Duration::from_millis(1000),
            request_size: None,
            response_size: None,
            count: None,
            run_time: None,
        }
    }
}

impl SessionConfig {
    /// Sets the fixed send interval; ignored in adaptive mode.
    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Sends the next request after the previous reply or timeout.
    pub fn adaptive(mut self) -> Self {
        self.adaptive = true;
        self
    }

    /// Sets how long to wait for each reply.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the request payload size.
    pub fn request_size(mut self, size: PacketSize) -> Self {
        self.request_size = Some(size);
        self
    }

    /// Asks a UDP/TCP echo server for a response of this size.
    pub fn response_size(mut self, size: PacketSize) -> Self {
        self.response_size = Some(size);
        self
    }

    /// Stops generation after `count` requests.
    pub fn count(mut self, count: u64) -> Self {
        self.count = Some(count);
        self
    }

    /// Stops generation after `duration`.
    pub fn run_time(mut self, duration: Duration) -> Self {
        self.run_time = Some(duration);
        self
    }
}

/// Builder for ping sessions over the built-in transports.
///
/// # Example
///
/// ```no_run
/// use rup::{Pinger, SessionConfig, Target};
///
/// # async fn example() -> std::io::Result<()> {
/// let session = SessionConfig::default()
///     .count(5)
///     .interval(std::time::Duration::from_secs(1));
/// let report = Pinger::new(Target::Udp("127.0.0.1:5000".parse().unwrap()))
///     .session(session)
///     .run()
///     .await?;
/// println!("min/avg/max = {:?}/{:?}/{:?}", report.min(), report.mean(), report.max());
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct Pinger {
    target: Target,
    local: SocketAddr,
    traffic_class: Option<TrafficClass>,
    session: SessionConfig,
}

impl Pinger {
    /// Creates a pinger for `target` with [`SessionConfig::default`] settings.
    pub fn new(target: Target) -> Self {
        Pinger {
            local: unspecified_address(target.remote()),
            target,
            traffic_class: None,
            session: SessionConfig::default(),
        }
    }

    /// Binds the client socket to `addr` instead of an unspecified address.
    pub fn local(mut self, addr: SocketAddr) -> Self {
        self.local = addr;
        self
    }

    /// Sets the outgoing IP TOS / IPv6 traffic class byte.
    pub fn traffic_class(mut self, traffic_class: TrafficClass) -> Self {
        self.traffic_class = Some(traffic_class);
        self
    }

    /// Replaces the default session settings.
    pub fn session(mut self, session: SessionConfig) -> Self {
        self.session = session;
        self
    }

    /// Runs the session to completion and returns its report.
    pub async fn run(self) -> io::Result<PingReport> {
        let report = spawn_ping_session(self, None, None).await?;
        wait_ping_report(report).await
    }

    /// Connects and returns a session that yields live events.
    pub async fn start(self) -> io::Result<PingSession> {
        let (events_send, events_recv) = mpsc::channel(CHANNEL_CAP);
        let (stop_send, stop_recv) = oneshot::channel();
        let report = spawn_ping_session(self, Some(events_send), Some(stop_recv)).await?;
        Ok(PingSession::new(events_recv, report, stop_send))
    }
}

/// Runs a session over a caller-provided transport and returns its report.
pub async fn run_with_transport<T>(transport: T, session: SessionConfig) -> io::Result<PingReport>
where
    T: Transport + Clone + Send + 'static,
{
    run_session(transport, session, None, None).await
}

/// Starts a session over a caller-provided transport. Must be called within a Tokio runtime.
pub fn start_with_transport<T>(transport: T, session: SessionConfig) -> PingSession
where
    T: Transport + Clone + Send + 'static,
{
    let (events_send, events_recv) = mpsc::channel(CHANNEL_CAP);
    let (stop_send, stop_recv) = oneshot::channel();
    PingSession::new(
        events_recv,
        spawn_ping_with_transport(transport, session, Some(events_send), Some(stop_recv)),
        stop_send,
    )
}

async fn wait_ping_report(report: JoinHandle<io::Result<PingReport>>) -> io::Result<PingReport> {
    report
        .await
        .map_err(|e| io::Error::other(format!("ping session task failed: {e}")))?
}

async fn spawn_ping_session(
    pinger: Pinger,
    events: Option<mpsc::Sender<PingEvent>>,
    stop: Option<oneshot::Receiver<()>>,
) -> io::Result<JoinHandle<io::Result<PingReport>>> {
    let Pinger {
        target,
        local,
        traffic_class,
        session,
    } = pinger;

    tracing::debug!(?target, "creating transport");
    match target {
        Target::Udp(remote) => {
            let transport = UdpClientTransport::new_with_tos(local, remote, traffic_class).await?;
            Ok(spawn_ping_with_transport(transport, session, events, stop))
        }
        Target::Tcp(remote) => {
            let transport =
                TcpClientTransport::connect_or_probe(local, remote, traffic_class, session.timeout)
                    .await?;
            Ok(spawn_ping_with_transport(transport, session, events, stop))
        }
        Target::Icmp(remote) => {
            let transport = IcmpClientTransport::new_with_tos(local, remote, traffic_class).await?;
            Ok(spawn_ping_with_transport(transport, session, events, stop))
        }
        Target::Http {
            version,
            remote,
            url,
        } => {
            let transport = HttpClientTransport::connect(
                version,
                local,
                remote,
                traffic_class,
                url,
                session.timeout,
            )
            .await?;
            Ok(spawn_ping_with_transport(transport, session, events, stop))
        }
    }
}

fn unspecified_address(remote: SocketAddr) -> SocketAddr {
    let ip = match remote {
        SocketAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        SocketAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    SocketAddr::new(ip, 0)
}

fn spawn_ping_with_transport<T>(
    transport: T,
    session: SessionConfig,
    events: Option<mpsc::Sender<PingEvent>>,
    stop: Option<oneshot::Receiver<()>>,
) -> JoinHandle<io::Result<PingReport>>
where
    T: Transport + Clone + Send + 'static,
{
    tokio::spawn(
        run_session(transport, session, events, stop).instrument(tracing::info_span!("session")),
    )
}

async fn run_session<T>(
    transport: T,
    session: SessionConfig,
    events: Option<mpsc::Sender<PingEvent>>,
    stop: Option<oneshot::Receiver<()>>,
) -> io::Result<PingReport>
where
    T: Transport + Clone + Send + 'static,
{
    let channels = SessionChannels::new();
    let expected_requests = session.count;
    tracing::debug!(
        count = ?session.count,
        interval = ?session.interval,
        adaptive = session.adaptive,
        timeout = ?session.timeout,
        "session starting"
    );
    let transmitter_span = tracing::info_span!("transmitter");
    let receiver_span = tracing::info_span!("receiver");
    let generator_span = tracing::info_span!("generator");
    let statistics_span = tracing::info_span!("statistics");

    let (send_mode, txtr_gen) = if session.adaptive {
        let (txtr_gen_send, txtr_gen_recv) = mpsc::channel(CHANNEL_CAP);
        (SendMode::Adaptive(txtr_gen_recv), Some(txtr_gen_send))
    } else {
        (SendMode::Interval(session.interval), None)
    };

    let (sends_done_send, sends_done_recv) = mpsc::channel(1);
    let mut tasks = JoinSet::new();

    let transmitter_transport = transport.clone();
    let statistics_sender = channels.transport_to_statista.clone();
    tasks.spawn(
        async move {
            let result = transmitter(
                transmitter_transport,
                channels.transmitter_input,
                statistics_sender,
            )
            .await;
            let _ = sends_done_send.send(()).await;
            SessionTask::Transmitter(result)
        }
        .instrument(transmitter_span),
    );

    tasks.spawn(
        async move {
            SessionTask::Receiver(receiver(transport, channels.transport_to_statista).await)
        }
        .instrument(receiver_span),
    );

    let generator_config = pinger::GeneratorConfig {
        send_mode,
        ping_number: session.count,
        run_time: session.run_time,
        request_size: session.request_size,
        response_size: session.response_size,
    };
    let generator_task = tasks.spawn(
        async move {
            let run = pinger::generator(channels.generator_to_transmitter, generator_config);
            if let Some(stop) = stop {
                tokio::select! {
                    _ = run => {}
                    _ = stop => {}
                }
            } else {
                run.await;
            }
            SessionTask::Generator
        }
        .instrument(generator_span),
    );

    if let Some(events) = events {
        tasks.spawn(
            async move {
                SessionTask::Statistics(
                    statistics::statista_with_events_and_send_done(
                        channels.statista_input,
                        sends_done_recv,
                        txtr_gen,
                        session.timeout,
                        events,
                    )
                    .await,
                )
            }
            .instrument(statistics_span),
        );
    } else {
        tasks.spawn(
            async move {
                SessionTask::Statistics(
                    statistics::statista_report_with_send_done(
                        channels.statista_input,
                        sends_done_recv,
                        txtr_gen,
                        session.timeout,
                    )
                    .await,
                )
            }
            .instrument(statistics_span),
        );
    }

    let mut receiver_error = None;
    while let Some(task) = tasks.join_next().await {
        let task = match task {
            Ok(task) => task,
            Err(error) if error.is_cancelled() && receiver_error.is_some() => continue,
            Err(error) => {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                return Err(io::Error::other(format!("session task failed: {error}")));
            }
        };

        match task {
            SessionTask::Generator | SessionTask::Transmitter(Ok(())) => {}
            SessionTask::Transmitter(Err(error)) => {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                return Err(error);
            }
            SessionTask::Receiver(result) => {
                receiver_error = Some(result.err().unwrap_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "receiver stopped before session completion",
                    )
                }));
                generator_task.abort();
            }
            SessionTask::Statistics(report) => {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                if let Some(error) = receiver_error
                    && (expected_requests != Some(report.sent) || report.received != report.sent)
                {
                    tracing::debug!(
                        sent = report.sent,
                        received = report.received,
                        "session failed"
                    );
                    return Err(error);
                }
                tracing::debug!(
                    sent = report.sent,
                    received = report.received,
                    "session finished"
                );
                return Ok(report);
            }
        }
    }

    Err(receiver_error
        .unwrap_or_else(|| io::Error::other("ping session stopped without producing a report")))
}

/// Runs an echo server until the process ends.
///
/// Supports [`Protocol::Udp`] and [`Protocol::Tcp`]; other protocols return an
/// [`io::ErrorKind::InvalidInput`] error.
pub async fn run_server(protocol: Protocol, local: SocketAddr) -> io::Result<()> {
    run_server_until(protocol, local, std::future::pending()).await
}

/// Runs an echo server until `shutdown` completes.
///
/// Supports [`Protocol::Udp`] and [`Protocol::Tcp`]; other protocols return an
/// [`io::ErrorKind::InvalidInput`] error.
pub async fn run_server_until(
    protocol: Protocol,
    local: SocketAddr,
    shutdown: impl std::future::Future<Output = ()>,
) -> io::Result<()> {
    match protocol {
        Protocol::Tcp => transport::async_tcp::server_transport_until(local, shutdown).await?,
        Protocol::Udp => transport::async_udp::server_transport_until(local, shutdown).await?,
        Protocol::Icmp | Protocol::Http1 | Protocol::Http2 => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("there is no server for {protocol}"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Request, Response};
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::{Mutex, mpsc};

    /// Replies to the scripted ids, in order, as each one is sent.
    #[derive(Clone)]
    struct ScriptedTransport {
        replies: Arc<Mutex<VecDeque<u64>>>,
        reply_delay: Duration,
        delivered_tx: mpsc::UnboundedSender<u64>,
        delivered_rx: Arc<Mutex<mpsc::UnboundedReceiver<u64>>>,
    }

    #[derive(Clone)]
    struct SendErrorTransport;

    #[derive(Clone)]
    struct ReceiveErrorTransport;

    impl ScriptedTransport {
        fn new(responses: impl IntoIterator<Item = u64>) -> Self {
            let (delivered_tx, delivered_rx) = mpsc::unbounded_channel();
            Self {
                replies: Arc::new(Mutex::new(responses.into_iter().collect())),
                reply_delay: Duration::ZERO,
                delivered_tx,
                delivered_rx: Arc::new(Mutex::new(delivered_rx)),
            }
        }

        fn with_reply_delay(mut self, delay: Duration) -> Self {
            self.reply_delay = delay;
            self
        }
    }

    impl Transport for ScriptedTransport {
        async fn send(&self, req: &Request) -> io::Result<Instant> {
            let mut replies = self.replies.lock().await;
            if replies.front() == Some(&req.id) {
                replies.pop_front();
                let (id, delay, delivered) = (req.id, self.reply_delay, self.delivered_tx.clone());
                // Deliver after send returns so the reply follows its Open entry.
                tokio::spawn(async move {
                    if delay.is_zero() {
                        tokio::task::yield_now().await;
                    } else {
                        tokio::time::sleep(delay).await;
                    }
                    let _ = delivered.send(id);
                });
            }
            Ok(Instant::now())
        }

        async fn recv(&self) -> io::Result<Response> {
            let id = self.delivered_rx.lock().await.recv().await;
            // The sender half lives in `self`, so the channel never closes.
            let id = id.expect("scripted transport channel closed");
            Ok(Response {
                id,
                timestamp: Instant::now(),
                size: 0,
                ttl: None,
            })
        }
    }

    impl Transport for SendErrorTransport {
        async fn send(&self, _req: &Request) -> io::Result<Instant> {
            Err(io::Error::other("scripted send failure"))
        }

        async fn recv(&self) -> io::Result<Response> {
            std::future::pending().await
        }
    }

    impl Transport for ReceiveErrorTransport {
        async fn send(&self, _req: &Request) -> io::Result<Instant> {
            Ok(Instant::now())
        }

        async fn recv(&self) -> io::Result<Response> {
            Err(io::Error::other("scripted receive failure"))
        }
    }

    #[test]
    fn has_port_requires_a_numeric_port_after_the_host() {
        for (address, expected) in [
            ("127.0.0.1:5000", true),
            ("127.0.0.1", false),
            ("[::1]:5000", true),
            ("[::1]", false),
            ("[2001:db8::1]:8080", true),
            ("[2001:db8::1]", false),
            ("localhost:8080", true),
            ("localhost", false),
            ("127.0.0.1:", false),
            ("127.0.0.1:abc", false),
            ("", false),
            (":5000", true),
        ] {
            assert_eq!(has_port(address), expected, "{address:?}");
        }
    }

    #[test]
    fn ensure_port_keeps_ports_and_adds_zero_for_icmp() {
        for (address, protocol, expected) in [
            ("10.0.0.1:9999", Protocol::Udp, "10.0.0.1:9999"),
            ("[::1]:443", Protocol::Tcp, "[::1]:443"),
            ("192.168.1.1", Protocol::Icmp, "192.168.1.1:0"),
            ("[::1]", Protocol::Icmp, "[::1]:0"),
            ("::1", Protocol::Icmp, "[::1]:0"),
            ("localhost", Protocol::Icmp, "localhost:0"),
        ] {
            assert_eq!(
                ensure_port(address, protocol).unwrap(),
                expected,
                "{address:?}"
            );
        }
    }

    #[test]
    fn ensure_port_uses_http_url_host_and_default_port() {
        assert_eq!(
            ensure_port("https://example.com/path", Protocol::Http2).unwrap(),
            "example.com:443"
        );
        assert_eq!(
            ensure_port("http://[::1]:8080", Protocol::Http1).unwrap(),
            "[::1]:8080"
        );
        assert_eq!(
            ensure_port("ftp://example.com", Protocol::Http1)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
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
    fn pinger_default_local_address_matches_remote_family() {
        let ipv4 = Pinger::new(Target::Udp("127.0.0.1:5000".parse().unwrap()));
        let ipv6 = Pinger::new(Target::Udp("[::1]:5000".parse().unwrap()));

        assert_eq!(ipv4.local, "0.0.0.0:0".parse().unwrap());
        assert_eq!(ipv6.local, "[::]:0".parse().unwrap());
    }

    #[test]
    fn pinger_default_local_address_uses_http_remote_family() {
        let remote = "[::1]:8080".parse().unwrap();
        let pinger = Pinger::new(Target::Http {
            version: HttpVersion::Http2,
            remote,
            url: HttpTarget::from_socket_addr(remote),
        });

        assert_eq!(pinger.local, "[::]:0".parse().unwrap());
    }

    /// Starts the rup echo server on a free loopback port.
    async fn spawn_rup_server(
        protocol: Protocol,
    ) -> (
        SocketAddr,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        let address = match protocol {
            Protocol::Udp => std::net::UdpSocket::bind("127.0.0.1:0").and_then(|s| s.local_addr()),
            _ => std::net::TcpListener::bind("127.0.0.1:0").and_then(|s| s.local_addr()),
        }
        .unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(run_server_until(protocol, address, async {
            let _ = shutdown_rx.await;
        }));
        // The current-thread test runtime polls the server task here, and it
        // binds before its first await point that can pend.
        tokio::task::yield_now().await;
        (address, shutdown_tx, server)
    }

    #[tokio::test]
    async fn pinger_against_rup_server_echoes_requested_sizes() {
        let request_size = PacketSize::new(40).unwrap();
        let response_size = PacketSize::new(100).unwrap();

        for protocol in [Protocol::Udp, Protocol::Tcp] {
            for (requested_response, expected_size) in [(None, 40), (Some(response_size), 100)] {
                let (address, shutdown, server) = spawn_rup_server(protocol).await;
                let target = match protocol {
                    Protocol::Udp => Target::Udp(address),
                    _ => Target::Tcp(address),
                };
                let mut config = SessionConfig::default()
                    .interval(Duration::from_millis(1))
                    .timeout(Duration::from_millis(500))
                    .request_size(request_size)
                    .count(3);
                if let Some(size) = requested_response {
                    config = config.response_size(size);
                }

                let mut session = Pinger::new(target).session(config).start().await.unwrap();
                let mut sizes = Vec::new();
                while let Some(event) = session.next().await {
                    match event {
                        PingEvent::Reply(result) => sizes.push(result.size),
                        other => panic!("{protocol}: unexpected {other:?}"),
                    }
                }
                let report = session.report().await.unwrap();

                assert_eq!(sizes, vec![expected_size; 3], "{protocol}");
                assert_eq!(report.sent, 3);
                assert_eq!(report.received, 3);
                shutdown.send(()).unwrap();
                server.await.unwrap().unwrap();
            }
        }
    }

    #[tokio::test]
    async fn pinger_run_udp_no_response_reports_loss() {
        use tokio::net::UdpSocket;

        let server_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let _ = server_sock.recv_from(&mut buf).await;
        });

        let report = Pinger::new(Target::Udp(server_addr))
            .session(
                SessionConfig::default()
                    .interval(Duration::from_millis(1))
                    .timeout(Duration::from_millis(5))
                    .count(1),
            )
            .run()
            .await
            .unwrap();

        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 0);
        assert_eq!(report.loss_pct(), 100.0);
        server.await.unwrap();
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn pinger_run_udp_closed_port_uses_icmp_error() {
        use tokio::net::UdpSocket;

        let unused_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let unused_addr = unused_socket.local_addr().unwrap();
        drop(unused_socket);

        let report = Pinger::new(Target::Udp(unused_addr))
            .session(
                SessionConfig::default()
                    .interval(Duration::from_millis(1))
                    .timeout(Duration::from_millis(100))
                    .count(1),
            )
            .run()
            .await
            .unwrap();

        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 1);
        assert_eq!(report.loss_pct(), 0.0);
    }

    #[tokio::test]
    async fn pinger_run_tcp_closed_port_advances_adaptive_mode() {
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = listener.local_addr().unwrap();
        drop(listener);

        let report = Pinger::new(Target::Tcp(remote))
            .session(
                SessionConfig::default()
                    .interval(Duration::from_millis(1))
                    .adaptive()
                    .timeout(Duration::from_millis(100))
                    .count(2),
            )
            .run()
            .await
            .unwrap();

        assert_eq!(report.sent, 2);
        assert_eq!(report.received, 2);
        assert_eq!(report.loss_pct(), 0.0);
    }

    #[tokio::test]
    async fn run_ping_with_transport_reports_success() {
        let report = run_with_transport(
            ScriptedTransport::new([0, 1, 2]),
            SessionConfig::default()
                .interval(Duration::from_millis(1))
                .timeout(Duration::from_millis(100))
                .count(3),
        )
        .await
        .unwrap();

        assert_eq!(report.sent, 3);
        assert_eq!(report.received, 3);
        assert_eq!(report.rtts.len(), 3);
    }

    #[tokio::test]
    async fn run_ping_with_transport_propagates_send_failure() {
        let error = run_with_transport(
            SendErrorTransport,
            SessionConfig::default()
                .interval(Duration::from_millis(1))
                .timeout(Duration::from_millis(1))
                .count(1),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("scripted send failure"));
    }

    #[tokio::test]
    async fn run_ping_with_transport_propagates_receive_failure() {
        let error = run_with_transport(
            ReceiveErrorTransport,
            SessionConfig::default()
                .interval(Duration::from_millis(1))
                .timeout(Duration::from_millis(1))
                .count(1),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("scripted receive failure"));
    }

    #[tokio::test]
    async fn run_ping_with_transport_handles_many_responses() {
        let count = 2050;
        let report = tokio::time::timeout(
            Duration::from_secs(2),
            run_with_transport(
                ScriptedTransport::new(0..count),
                SessionConfig::default()
                    .adaptive()
                    .timeout(Duration::from_millis(100))
                    .count(count),
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
        let report = run_with_transport(
            ScriptedTransport::new([]),
            SessionConfig::default()
                .interval(Duration::from_millis(1))
                .timeout(Duration::from_millis(2))
                .count(2),
        )
        .await
        .unwrap();

        assert_eq!(report.sent, 2);
        assert_eq!(report.received, 0);
        assert_eq!(report.loss_pct(), 100.0);
    }

    #[tokio::test]
    async fn run_ping_with_transport_waits_for_delayed_final_response() {
        let report = run_with_transport(
            ScriptedTransport::new([0]).with_reply_delay(Duration::from_millis(10)),
            SessionConfig::default()
                .interval(Duration::from_millis(1))
                .timeout(Duration::from_millis(100))
                .count(1),
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
        let report = run_with_transport(
            ScriptedTransport::new([]),
            SessionConfig::default()
                .interval(Duration::from_millis(1))
                .timeout(wait_time)
                .count(1),
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
        let report = run_with_transport(
            ScriptedTransport::new(0..5),
            SessionConfig::default()
                .interval(Duration::from_millis(10))
                .timeout(wait_time)
                .count(10),
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
        let report = run_with_transport(
            ScriptedTransport::new([0, 1, 2]),
            SessionConfig::default()
                .adaptive()
                .timeout(Duration::from_millis(100))
                .count(3),
        )
        .await
        .unwrap();

        assert_eq!(report.sent, 3);
        assert_eq!(report.received, 3);
    }

    #[tokio::test]
    async fn ping_session_yields_reply_events_then_report() {
        let mut session = start_with_transport(
            ScriptedTransport::new([0, 1]),
            SessionConfig::default()
                .interval(Duration::from_millis(1))
                .timeout(Duration::from_millis(100))
                .count(2),
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
    async fn ping_session_stop_finishes_unbounded_session_with_report() {
        let mut session = start_with_transport(
            ScriptedTransport::new([0]),
            SessionConfig::default().timeout(Duration::from_millis(100)),
        );

        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(100), session.next())
                .await
                .unwrap(),
            Some(PingEvent::Reply(PingResult { seq: 0, .. }))
        ));

        session.stop();
        while session.next().await.is_some() {}
        let report = tokio::time::timeout(Duration::from_millis(200), session.report())
            .await
            .expect("stopped session should finish")
            .unwrap();

        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 1);
    }

    #[tokio::test]
    async fn ping_session_reports_timeouts_for_ten_sent_five_received_case() {
        let mut session = start_with_transport(
            ScriptedTransport::new(0..5),
            SessionConfig::default()
                .interval(Duration::from_millis(10))
                .timeout(Duration::from_millis(120))
                .count(10),
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
