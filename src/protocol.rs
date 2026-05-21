use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protocol {
    Udp,
    Tcp,
    Icmp,
}

impl Protocol {
    pub const VALUES: [&'static str; 3] = ["tcp", "udp", "icmp"];

    pub fn requires_port(self) -> bool {
        matches!(self, Protocol::Udp | Protocol::Tcp)
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Protocol::Udp => "udp",
            Protocol::Tcp => "tcp",
            Protocol::Icmp => "icmp",
        };
        f.write_str(value)
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
