use rup::{PingResult, Protocol};
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Field {
    Target,
    Ip,
    Seq,
    Rtt,
    RttMs,
    Size,
    Ttl,
    Status,
    Protocol,
}

impl FromStr for Field {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "target" => Ok(Self::Target),
            "ip" => Ok(Self::Ip),
            "seq" => Ok(Self::Seq),
            "rtt" => Ok(Self::Rtt),
            "rtt_ms" => Ok(Self::RttMs),
            "size" => Ok(Self::Size),
            "ttl" => Ok(Self::Ttl),
            "status" => Ok(Self::Status),
            "protocol" => Ok(Self::Protocol),
            _ => Err(format!("unknown format field '{value}'")),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Part {
    Literal(String),
    Field(Field),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReplyFormat {
    parts: Vec<Part>,
}

impl FromStr for ReplyFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.contains(['\r', '\n']) {
            return Err("format must not contain newlines".to_owned());
        }

        let mut chars = value.chars().peekable();
        let mut parts = Vec::new();
        let mut literal = String::new();

        while let Some(ch) = chars.next() {
            match ch {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    literal.push('{');
                }
                '{' => {
                    if !literal.is_empty() {
                        parts.push(Part::Literal(std::mem::take(&mut literal)));
                    }

                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some('{') => {
                                return Err("unmatched '{' in format".to_owned());
                            }
                            Some(ch) => name.push(ch),
                            None => return Err("unmatched '{' in format".to_owned()),
                        }
                    }
                    parts.push(Part::Field(name.parse()?));
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    literal.push('}');
                }
                '}' => return Err("unmatched '}' in format".to_owned()),
                ch => literal.push(ch),
            }
        }

        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }

        Ok(Self { parts })
    }
}

impl ReplyFormat {
    pub(crate) fn render(
        &self,
        target: &str,
        ip: IpAddr,
        protocol: Protocol,
        result: &PingResult,
    ) -> String {
        let mut output = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(value) => output.push_str(value),
                Part::Field(field) => match field {
                    Field::Target => output.push_str(target),
                    Field::Ip => write_value(&mut output, ip),
                    Field::Seq => write_value(&mut output, result.seq),
                    Field::Rtt => {
                        output.push_str(&fmt_duration_ms_value(result.rtt));
                        output.push_str(" ms");
                    }
                    Field::RttMs => output.push_str(&fmt_duration_ms_value(result.rtt)),
                    Field::Size => write_value(&mut output, result.size),
                    Field::Ttl => match result.ttl {
                        Some(ttl) => write_value(&mut output, ttl),
                        None => output.push('-'),
                    },
                    Field::Status => output.push_str(if result.size == 0 {
                        "terminal_reply"
                    } else {
                        "reply"
                    }),
                    Field::Protocol => output.push_str(protocol.as_str()),
                },
            }
        }
        output
    }
}

pub(crate) fn fmt_duration_ms_value(duration: Duration) -> String {
    format!("{:.3}", duration.as_secs_f64() * 1000.0)
}

fn write_value(output: &mut String, value: impl fmt::Display) {
    use fmt::Write;

    write!(output, "{value}").expect("writing to a String cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_all_reply_fields_and_escaped_braces() {
        let format: ReplyFormat =
            "{target}|{ip}|{seq}|{rtt}|{rtt_ms}|{size}|{ttl}|{status}|{protocol}|{{}}"
                .parse()
                .unwrap();
        let result = PingResult {
            seq: 7,
            rtt: Duration::from_micros(42),
            size: 56,
            ttl: Some(64),
        };

        assert_eq!(
            format.render(
                "example.test",
                "192.0.2.1".parse().unwrap(),
                Protocol::Icmp,
                &result,
            ),
            "example.test|192.0.2.1|7|0.042 ms|0.042|56|64|reply|icmp|{}"
        );
    }

    #[test]
    fn renders_terminal_reply_status_and_missing_ttl() {
        let format: ReplyFormat = "{status} size={size} ttl={ttl}".parse().unwrap();
        let result = PingResult {
            seq: 3,
            rtt: Duration::from_micros(75),
            size: 0,
            ttl: None,
        };

        assert_eq!(
            format.render(
                "127.0.0.1:5000",
                "127.0.0.1".parse().unwrap(),
                Protocol::Udp,
                &result,
            ),
            "terminal_reply size=0 ttl=-"
        );
    }

    #[test]
    fn rejects_unknown_placeholder() {
        let error = "{rtt_us}".parse::<ReplyFormat>().unwrap_err();

        assert!(error.contains("unknown format field 'rtt_us'"));
    }

    #[test]
    fn rejects_precision_modifiers() {
        let error = "{rtt_ms:.2}".parse::<ReplyFormat>().unwrap_err();

        assert!(error.contains("unknown format field 'rtt_ms:.2'"));
    }

    #[test]
    fn rejects_unmatched_braces() {
        assert!("{seq".parse::<ReplyFormat>().is_err());
        assert!("seq}".parse::<ReplyFormat>().is_err());
    }

    #[test]
    fn rejects_embedded_newlines() {
        assert!("{ip}\n{seq}".parse::<ReplyFormat>().is_err());
        assert!("{ip}\r{seq}".parse::<ReplyFormat>().is_err());
    }
}
