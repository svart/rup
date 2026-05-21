pub mod echo_codec;
pub mod pinger;
pub mod protocol;
pub mod statistics;
pub mod transport;

pub use pinger::{Echo, Entry, PING_HDR_LEN, Request, Response, SendMode, StatEntry, generator};
pub use protocol::Protocol;
pub use statistics::{
    RttSequence, statista, statista_with_collector, statista_with_presenter_and_collector,
};
pub use transport::async_icmp::IcmpClientTransport;
pub use transport::async_tcp::TcpClientTransport;
pub use transport::async_udp::UdpClientTransport;
pub use transport::{Transport, receiver, transmitter};

use std::io;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::mpsc;

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
        return Ok(format!("{addr}:0"));
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{protocol} requires a port (e.g. {addr}:PORT)"),
    ))
}

/// A single RTT measurement result.
#[derive(Debug, Clone)]
pub struct PingResult {
    pub seq: u64,
    pub rtt: Duration,
}

/// Summary report from a completed ping session.
#[derive(Debug, Clone)]
pub struct PingReport {
    pub rtts: Vec<Duration>,
    pub sent: u64,
    pub received: u64,
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

/// High-level builder for quick ping sessions.
///
/// # Example
///
/// ```no_run
/// use rup::Pinger;
///
/// # async fn example() -> std::io::Result<()> {
/// let report = Pinger::new("127.0.0.1:5000", "udp")
///     .count(5)
///     .interval(1000)
///     .run()
///     .await?;
/// println!("min/avg/max = {:?}/{:?}/{:?}", report.min(), report.mean(), report.max());
/// # Ok(())
/// # }
/// ```
pub struct Pinger {
    remote: String,
    local: SocketAddr,
    protocol: String,
    interval: u64,
    adaptive: bool,
    wait_time: u64,
    request_size: Option<u16>,
    response_size: Option<u16>,
    ping_number: Option<u64>,
    run_time: Option<Duration>,
}

impl Pinger {
    pub fn new<S: Into<String>>(remote: S, protocol: S) -> Self {
        Pinger {
            remote: remote.into(),
            local: "0.0.0.0:0".parse().unwrap(),
            protocol: protocol.into(),
            interval: 1000,
            adaptive: false,
            wait_time: 1000,
            request_size: None,
            response_size: None,
            ping_number: None,
            run_time: None,
        }
    }

    pub fn count(mut self, n: u64) -> Self {
        self.ping_number = Some(n);
        self
    }

    pub fn interval(mut self, ms: u64) -> Self {
        self.interval = ms;
        self
    }

    pub fn adaptive(mut self) -> Self {
        self.adaptive = true;
        self
    }

    pub fn wait_time(mut self, ms: u64) -> Self {
        self.wait_time = ms;
        self
    }

    pub fn request_size(mut self, size: u16) -> Self {
        self.request_size = Some(size);
        self
    }

    pub fn response_size(mut self, size: u16) -> Self {
        self.response_size = Some(size);
        self
    }

    pub fn local(mut self, addr: SocketAddr) -> Self {
        self.local = addr;
        self
    }

    pub fn run_time(mut self, dur: Duration) -> Self {
        self.run_time = Some(dur);
        self
    }

    pub async fn run(self) -> io::Result<PingReport> {
        let protocol = Protocol::from_str(&self.protocol)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        run_ping_session(PingConfig {
            remote: self.remote,
            local: self.local,
            protocol,
            interval: self.interval,
            adaptive: self.adaptive,
            wait_time: Duration::from_millis(self.wait_time),
            request_size: self.request_size,
            response_size: self.response_size,
            ping_number: self.ping_number,
            run_time: self.run_time,
        })
        .await
    }
}

#[derive(Clone, Debug)]
pub struct PingConfig {
    pub remote: String,
    pub local: SocketAddr,
    pub protocol: Protocol,
    pub interval: u64,
    pub adaptive: bool,
    pub wait_time: Duration,
    pub request_size: Option<u16>,
    pub response_size: Option<u16>,
    pub ping_number: Option<u64>,
    pub run_time: Option<Duration>,
}

impl PingConfig {
    pub fn new(remote: String, protocol: Protocol) -> Self {
        Self {
            remote,
            local: "0.0.0.0:0".parse().unwrap(),
            protocol,
            interval: 1000,
            adaptive: false,
            wait_time: Duration::from_millis(1000),
            request_size: None,
            response_size: None,
            ping_number: None,
            run_time: None,
        }
    }
}

pub async fn run_ping_session(config: PingConfig) -> io::Result<PingReport> {
    run_ping_session_inner(config, false).await
}

pub async fn run_ping_session_with_output(config: PingConfig) -> io::Result<PingReport> {
    run_ping_session_inner(config, true).await
}

async fn run_ping_session_inner(config: PingConfig, print_output: bool) -> io::Result<PingReport> {
    let channel_cap = 1024;
    let (gen_txtr_send, gen_txtr_recv) = mpsc::channel(channel_cap);
    let (txtr_stat_send, txtr_stat_recv) = mpsc::channel(channel_cap);
    let (result_send, mut result_recv) = mpsc::channel::<PingResult>(channel_cap);

    let addr = ensure_port(&config.remote, config.protocol)?;
    let remote_addr = match tokio::net::lookup_host(&addr).await {
        Ok(mut addrs) => match addrs.next() {
            Some(a) => a,
            None => {
                return Err(io::Error::other(format!(
                    "no addresses found for {}",
                    config.remote
                )));
            }
        },
        Err(e) => {
            return Err(io::Error::other(format!(
                "failed to resolve '{}': {}",
                config.remote, e
            )));
        }
    };

    let (send_mode, txtr_gen) = if config.adaptive {
        let (txtr_gen_send, txtr_gen_recv) = mpsc::channel(channel_cap);
        (SendMode::Adaptive(txtr_gen_recv), Some(txtr_gen_send))
    } else {
        (SendMode::Interval(config.interval), None)
    };

    let (mut tx_handle, mut rx_handle) = match config.protocol {
        Protocol::Udp => {
            let transport = UdpClientTransport::new(config.local, remote_addr).await?;
            spawn_pinger_tasks(transport, gen_txtr_recv, txtr_stat_send)
        }
        Protocol::Tcp => {
            let sock = if remote_addr.is_ipv4() {
                tokio::net::TcpSocket::new_v4()?
            } else {
                tokio::net::TcpSocket::new_v6()?
            };
            sock.bind(config.local)?;
            let stream = sock.connect(remote_addr).await?;
            let transport = TcpClientTransport::new(stream);
            spawn_pinger_tasks(transport, gen_txtr_recv, txtr_stat_send)
        }
        Protocol::Icmp => {
            let transport = IcmpClientTransport::new(config.local, remote_addr).await?;
            spawn_pinger_tasks(transport, gen_txtr_recv, txtr_stat_send)
        }
    };

    let generator = tokio::spawn(pinger::generator(
        gen_txtr_send,
        send_mode,
        config.ping_number,
        config.run_time,
        config.request_size,
        config.response_size,
    ));

    let statista = if print_output {
        tokio::spawn(statistics::statista_with_presenter_and_collector(
            txtr_stat_recv,
            txtr_gen,
            config.wait_time,
            result_send,
        ))
    } else {
        tokio::spawn(statistics::statista_with_collector(
            txtr_stat_recv,
            txtr_gen,
            config.wait_time,
            result_send,
        ))
    };

    tokio::select! {
        _ = &mut tx_handle => {
            rx_handle.abort();
        }
        _ = &mut rx_handle => {}
    }

    drop(tx_handle);
    drop(rx_handle);

    while result_recv.recv().await.is_some() {}
    let _ = generator.await;
    let stat_report = statista
        .await
        .map_err(|e| io::Error::other(format!("statista task failed: {e}")))?;

    Ok(stat_report)
}

pub async fn run_server(protocol: Protocol, local: SocketAddr) -> io::Result<()> {
    match protocol {
        Protocol::Tcp => transport::async_tcp::server_transport(local).await?,
        Protocol::Udp => transport::async_udp::server_transport(local).await?,
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
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let t2 = transport.clone();
    let tx = tokio::spawn(transmitter(t2, gen_txtr_recv, txtr_stat_send.clone()));
    let rx = tokio::spawn(receiver(transport, txtr_stat_send));
    (tx, rx)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn pinger_errors_on_unknown_protocol() {
        let result = Pinger::new("127.0.0.1:5000", "unknown")
            .count(1)
            .run()
            .await;
        assert!(result.is_err());
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
