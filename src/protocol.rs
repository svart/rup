use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    Udp,
    Tcp,
    Icmp,
}

impl Protocol {
    pub const VALUES: [&'static str; 3] = [
        Protocol::Tcp.as_str(),
        Protocol::Udp.as_str(),
        Protocol::Icmp.as_str(),
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Protocol::Udp => "udp",
            Protocol::Tcp => "tcp",
            Protocol::Icmp => "icmp",
        }
    }

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
    }

    #[test]
    fn values_match_parser() {
        for value in Protocol::VALUES {
            assert!(value.parse::<Protocol>().is_ok());
        }
    }
}
