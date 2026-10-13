use rup::{PingResult, Protocol};
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;
use std::time::Duration;

const DEFAULT_MS_PRECISION: usize = 3;
const MAX_MS_PRECISION: usize = 6;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Field {
    Target,
    Ip,
    Seq,
    Rtt,
    RttMs { precision: usize },
    InterPacket,
    InterPacketMs { precision: usize },
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
            "rtt_ms" => Ok(Self::RttMs {
                precision: DEFAULT_MS_PRECISION,
            }),
            "inter_packet" => Ok(Self::InterPacket),
            "inter_packet_ms" => Ok(Self::InterPacketMs {
                precision: DEFAULT_MS_PRECISION,
            }),
            "size" => Ok(Self::Size),
            "ttl" => Ok(Self::Ttl),
            "status" => Ok(Self::Status),
            "protocol" => Ok(Self::Protocol),
            _ => {
                if let Some(precision) = value.strip_prefix("rtt_ms:.") {
                    return Ok(Self::RttMs {
                        precision: parse_ms_precision("rtt_ms", precision)?,
                    });
                }
                if let Some(precision) = value.strip_prefix("inter_packet_ms:.") {
                    return Ok(Self::InterPacketMs {
                        precision: parse_ms_precision("inter_packet_ms", precision)?,
                    });
                }

                Err(format!("unknown format field '{value}'"))
            }
        }
    }
}

fn parse_ms_precision(field: &str, precision: &str) -> Result<usize, String> {
    if precision.len() != 1 || !precision.as_bytes()[0].is_ascii_digit() {
        return Err(format!(
            "invalid {field} precision; expected {{{field}:.N}} with N from 0 to 6"
        ));
    }

    let precision = usize::from(precision.as_bytes()[0] - b'0');
    if precision > MAX_MS_PRECISION {
        return Err(format!("{field} precision must be between 0 and 6"));
    }
    Ok(precision)
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
                '\\' => match chars.next() {
                    Some('n') => literal.push('\n'),
                    Some('t') => literal.push('\t'),
                    Some('\\') => literal.push('\\'),
                    Some(ch) => {
                        return Err(format!(
                            "unsupported escape '\\{ch}' in format; expected \\n, \\t, or \\\\"
                        ));
                    }
                    None => {
                        return Err(
                            "trailing '\\' in format; use \\\\ for a literal backslash".to_owned()
                        );
                    }
                },
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
        inter_packet: Option<Duration>,
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
                    Field::RttMs { precision } => output.push_str(
                        &fmt_duration_ms_value_with_precision(result.rtt, *precision),
                    ),
                    Field::InterPacket => match inter_packet {
                        Some(interval) => {
                            output.push_str(&fmt_duration_ms_value(interval));
                            output.push_str(" ms");
                        }
                        None => output.push('-'),
                    },
                    Field::InterPacketMs { precision } => match inter_packet {
                        Some(interval) => output
                            .push_str(&fmt_duration_ms_value_with_precision(interval, *precision)),
                        None => output.push('-'),
                    },
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
    fmt_duration_ms_value_with_precision(duration, DEFAULT_MS_PRECISION)
}

fn fmt_duration_ms_value_with_precision(duration: Duration, precision: usize) -> String {
    format!(
        "{:.precision$}",
        duration.as_secs_f64() * 1000.0,
        precision = precision
    )
}

fn write_value(output: &mut String, value: impl fmt::Display) {
    use fmt::Write;

    write!(output, "{value}").expect("writing to a String cannot fail");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn renders_all_reply_fields_and_escaped_braces() {
        let format: ReplyFormat =
            "{target}|{ip}|{seq}|{rtt}|{rtt_ms}|{size}|{ttl}|{status}|{protocol}|{{}}"
                .parse()
                .unwrap();
        let result = PingResult {
            seq: 7,
            rtt: Duration::from_micros(42),
            received_at: Instant::now(),
            size: 56,
            ttl: Some(64),
        };

        assert_eq!(
            format.render(
                "example.test",
                "192.0.2.1".parse().unwrap(),
                Protocol::Icmp,
                &result,
                None,
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
            received_at: Instant::now(),
            size: 0,
            ttl: None,
        };

        assert_eq!(
            format.render(
                "127.0.0.1:5000",
                "127.0.0.1".parse().unwrap(),
                Protocol::Udp,
                &result,
                None,
            ),
            "terminal_reply size=0 ttl=-"
        );
    }

    #[test]
    fn renders_rtt_ms_with_precision_from_zero_through_six() {
        let format: ReplyFormat =
            "{rtt_ms}|{rtt_ms:.0}|{rtt_ms:.1}|{rtt_ms:.2}|{rtt_ms:.3}|{rtt_ms:.4}|{rtt_ms:.5}|{rtt_ms:.6}"
                .parse()
                .unwrap();
        let result = PingResult {
            seq: 1,
            rtt: Duration::from_nanos(1_234_567),
            received_at: Instant::now(),
            size: 56,
            ttl: Some(64),
        };

        assert_eq!(
            format.render(
                "example.test",
                "192.0.2.1".parse().unwrap(),
                Protocol::Icmp,
                &result,
                None,
            ),
            "1.235|1|1.2|1.23|1.235|1.2346|1.23457|1.234567"
        );
    }

    #[test]
    fn rejects_unknown_placeholder() {
        let error = "{rtt_us}".parse::<ReplyFormat>().unwrap_err();

        assert!(error.contains("unknown format field 'rtt_us'"));
    }

    #[test]
    fn rejects_rtt_ms_precision_above_six() {
        let error = "{rtt_ms:.7}".parse::<ReplyFormat>().unwrap_err();

        assert!(error.contains("precision must be between 0 and 6"));
    }

    #[test]
    fn rejects_malformed_or_unsupported_format_modifiers() {
        assert!("{rtt_ms:.}".parse::<ReplyFormat>().is_err());
        assert!("{rtt_ms:.02}".parse::<ReplyFormat>().is_err());
        assert!("{rtt_ms:2}".parse::<ReplyFormat>().is_err());
        assert!("{rtt:.2}".parse::<ReplyFormat>().is_err());
        assert!("{seq:.2}".parse::<ReplyFormat>().is_err());
    }

    #[test]
    fn rejects_unmatched_braces() {
        assert!("{seq".parse::<ReplyFormat>().is_err());
        assert!("seq}".parse::<ReplyFormat>().is_err());
    }

    #[test]
    fn renders_inter_packet_interval_fields() {
        let format: ReplyFormat =
            "{inter_packet}|{inter_packet_ms}|{inter_packet_ms:.0}|{inter_packet_ms:.6}"
                .parse()
                .unwrap();
        let result = PingResult {
            seq: 1,
            rtt: Duration::from_micros(42),
            received_at: Instant::now(),
            size: 56,
            ttl: Some(64),
        };

        assert_eq!(
            format.render(
                "example.test",
                "192.0.2.1".parse().unwrap(),
                Protocol::Icmp,
                &result,
                Some(Duration::from_nanos(459_123_456)),
            ),
            "459.123 ms|459.123|459|459.123456"
        );
    }

    #[test]
    fn renders_missing_inter_packet_interval_as_dash() {
        let format: ReplyFormat = "{inter_packet}|{inter_packet_ms}|{inter_packet_ms:.2}"
            .parse()
            .unwrap();
        let result = PingResult {
            seq: 0,
            rtt: Duration::from_micros(42),
            received_at: Instant::now(),
            size: 56,
            ttl: Some(64),
        };

        assert_eq!(
            format.render(
                "example.test",
                "192.0.2.1".parse().unwrap(),
                Protocol::Icmp,
                &result,
                None,
            ),
            "-|-|-"
        );
    }

    #[test]
    fn rejects_invalid_inter_packet_precision() {
        let error = "{inter_packet_ms:.7}".parse::<ReplyFormat>().unwrap_err();

        assert!(error.contains("inter_packet_ms precision must be between 0 and 6"));
        assert!("{inter_packet_ms:.}".parse::<ReplyFormat>().is_err());
        assert!("{inter_packet:.2}".parse::<ReplyFormat>().is_err());
    }

    #[test]
    fn renders_newline_tab_and_backslash_escapes() {
        let format: ReplyFormat = r"{seq}\n{rtt_ms}\t\\".parse().unwrap();
        let result = PingResult {
            seq: 7,
            rtt: Duration::from_micros(42),
            received_at: Instant::now(),
            size: 56,
            ttl: Some(64),
        };

        assert_eq!(
            format.render(
                "example.test",
                "192.0.2.1".parse().unwrap(),
                Protocol::Icmp,
                &result,
                None,
            ),
            "7\n0.042\t\\"
        );
    }

    #[test]
    fn accepts_literal_newlines() {
        assert!("{ip}\n{seq}".parse::<ReplyFormat>().is_ok());
    }

    #[test]
    fn rejects_unsupported_or_trailing_escapes() {
        let error = r"{seq}\r".parse::<ReplyFormat>().unwrap_err();
        assert!(error.contains(r"unsupported escape '\r'"));

        let error = r"{seq}\".parse::<ReplyFormat>().unwrap_err();
        assert!(error.contains("trailing"));
    }
}
