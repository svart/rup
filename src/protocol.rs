//! Ping protocol selection shared by the CLI and the library.

use std::fmt;
use std::str::FromStr;

use crate::http_target::HttpVersion;

/// Network protocol used for a ping session.
///
/// Parses the CLI spellings, where `http` is an alias for [`Protocol::Http1`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    /// UDP echo requests.
    Udp,
    /// TCP application echo or connect probes.
    Tcp,
    /// ICMP echo requests using Linux ping sockets.
    Icmp,
    /// HTTP/1.1 `GET` with a new connection per request.
    Http1,
    /// HTTP/2 `GET` over one persistent connection.
    Http2,
}

impl Protocol {
    /// Accepted CLI spellings, in help-output order.
    pub const VALUES: [&'static str; 5] = [
        Protocol::Tcp.as_str(),
        Protocol::Udp.as_str(),
        Protocol::Icmp.as_str(),
        Protocol::Http1.as_str(),
        Protocol::Http2.as_str(),
    ];

    /// Lowercase protocol name used in CLI arguments and output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Protocol::Udp => "udp",
            Protocol::Tcp => "tcp",
            Protocol::Icmp => "icmp",
            Protocol::Http1 => "http1",
            Protocol::Http2 => "http2",
        }
    }

    /// Returns the HTTP version for the HTTP protocols, or `None` otherwise.
    pub const fn http_version(self) -> Option<HttpVersion> {
        match self {
            Protocol::Http1 => Some(HttpVersion::Http1),
            Protocol::Http2 => Some(HttpVersion::Http2),
            Protocol::Udp | Protocol::Tcp | Protocol::Icmp => None,
        }
    }

    /// Whether addresses for this protocol must include an explicit port.
    pub fn requires_port(self) -> bool {
        matches!(self, Protocol::Udp | Protocol::Tcp)
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Protocol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "udp" => Ok(Protocol::Udp),
            "tcp" => Ok(Protocol::Tcp),
            "icmp" => Ok(Protocol::Icmp),
            "http" | "http1" => Ok(Protocol::Http1),
            "http2" => Ok(Protocol::Http2),
            _ => Err(format!("unknown protocol: {s}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_protocols() {
        assert_eq!("udp".parse::<Protocol>().unwrap(), Protocol::Udp);
        assert_eq!("tcp".parse::<Protocol>().unwrap(), Protocol::Tcp);
        assert_eq!("icmp".parse::<Protocol>().unwrap(), Protocol::Icmp);
        assert_eq!("http1".parse::<Protocol>().unwrap(), Protocol::Http1);
        assert_eq!("http2".parse::<Protocol>().unwrap(), Protocol::Http2);
    }

    #[test]
    fn http_is_an_alias_for_http1() {
        assert_eq!("http".parse::<Protocol>().unwrap(), Protocol::Http1);
    }

    #[test]
    fn http_version_matches_http_protocols() {
        assert_eq!(Protocol::Http1.http_version(), Some(HttpVersion::Http1));
        assert_eq!(Protocol::Http2.http_version(), Some(HttpVersion::Http2));
        assert_eq!(Protocol::Tcp.http_version(), None);
    }

    #[test]
    fn invalid_protocol_reports_value() {
        assert_eq!(
            "bad".parse::<Protocol>().unwrap_err(),
            "unknown protocol: bad"
        );
    }

    #[test]
    fn display_protocols() {
        assert_eq!(Protocol::Udp.to_string(), "udp");
        assert_eq!(Protocol::Tcp.to_string(), "tcp");
        assert_eq!(Protocol::Icmp.to_string(), "icmp");
    }

    #[test]
    fn port_requirement_matches_transport() {
        assert!(Protocol::Udp.requires_port());
        assert!(Protocol::Tcp.requires_port());
        assert!(!Protocol::Icmp.requires_port());
        assert!(!Protocol::Http1.requires_port());
        assert!(!Protocol::Http2.requires_port());
    }

    #[test]
    fn values_match_parser() {
        for value in Protocol::VALUES {
            assert!(value.parse::<Protocol>().is_ok());
        }
    }
}
