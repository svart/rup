//! A universal pinger library: RTT measurement over UDP, TCP, ICMP, and HTTP(S).
//!
//! Start with [`Pinger`] for the high-level session API, or implement
//! [`Transport`] and call [`run_with_transport`] for a custom protocol. See the
//! repository's `docs/library.md` for a guide and `docs/architecture.md` for the
//! internal pipeline.

#![warn(missing_docs)]

mod echo_codec;
mod http_target;
mod pinger;
pub mod protocol;
mod session;
mod statistics;
mod tos;
mod transport;
mod unix;

pub use http_target::{HttpScheme, HttpTarget, HttpVersion};
pub use pinger::{PacketSize, PacketSizeError, Request, Response};
pub use protocol::Protocol;
pub use session::{
    PingEvent, PingReport, PingReportError, PingResult, PingSession, Pinger, SessionConfig, Target,
    TrafficClass, ensure_port, has_port, run_server, run_server_until, run_with_transport,
    start_with_transport,
};
pub use transport::Transport;
#[cfg(feature = "http")]
pub use transport::async_http::HttpClientTransport as HttpTransport;
pub use transport::async_icmp::IcmpClientTransport as IcmpTransport;
pub use transport::async_tcp::TcpClientTransport as TcpTransport;
pub use transport::async_udp::UdpClientTransport as UdpTransport;
