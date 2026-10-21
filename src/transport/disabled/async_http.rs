//! Stand-in for `transport/async_http.rs` when the `http` feature is disabled.
//!
//! Keeps the same signatures so callers compile unchanged; connecting fails at
//! runtime with `ErrorKind::Unsupported`.

use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::TrafficClass;
use crate::http_target::{HttpTarget, HttpVersion};
use crate::pinger::{Request, Response};
use crate::transport::Transport;

#[derive(Clone)]
pub enum HttpClientTransport {}

impl HttpClientTransport {
    pub async fn connect(
        _version: HttpVersion,
        _local: SocketAddr,
        _remote: SocketAddr,
        _tos: Option<TrafficClass>,
        _target: HttpTarget,
        _wait_time: Duration,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "HTTP support is disabled; rebuild rup with the `http` feature",
        ))
    }
}

impl Transport for HttpClientTransport {
    async fn send(&self, _req: &Request) -> io::Result<Instant> {
        match *self {}
    }

    async fn recv(&self) -> io::Result<Response> {
        match *self {}
    }
}
