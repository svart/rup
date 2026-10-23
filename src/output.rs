//! Human-readable and JSON Lines rendering of ping events and reports.

use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::output_template::{ReplyFormat, fmt_duration_ms_value};
use rup::{PacketSize, PingEvent, PingReport, PingResult, Protocol};
use serde_json::json;

#[derive(Clone, Debug)]
pub(crate) struct ResolvedTarget {
    pub(crate) input: String,
    pub(crate) address: SocketAddr,
}

pub(crate) struct OutputContext {
    pub(crate) target: ResolvedTarget,
    pub(crate) protocol: Protocol,
    pub(crate) request_size: Option<PacketSize>,
}

fn data_size(ctx: &OutputContext) -> usize {
    ctx.request_size
        .map(PacketSize::get)
        .unwrap_or(PacketSize::MIN) as usize
}

fn total_packet_size(ctx: &OutputContext) -> usize {
    let ip_header = if ctx.target.address.is_ipv4() { 20 } else { 40 };
    let protocol_header = match ctx.protocol {
        Protocol::Icmp | Protocol::Udp => 8,
        Protocol::Tcp | Protocol::Http1 | Protocol::Http2 => 20,
    };
    data_size(ctx) + ip_header + protocol_header
}

pub(crate) fn header_line(ctx: &OutputContext) -> String {
    if ctx.protocol.http_version().is_some() {
        return format!(
            "PING {} ({}) via {}.",
            ctx.target.input, ctx.target.address, ctx.protocol
        );
    }
    format!(
        "PING {} ({}) {}({}) bytes of data.",
        ctx.target.input,
        ctx.target.address.ip(),
        data_size(ctx),
        total_packet_size(ctx)
    )
}

pub(crate) fn reply_line(
    ctx: &OutputContext,
    result: &PingResult,
    format: Option<&ReplyFormat>,
    inter_packet: Option<Duration>,
) -> String {
    if let Some(format) = format {
        return format.render(
            &ctx.target.input,
            ctx.target.address.ip(),
            ctx.protocol,
            result,
            inter_packet,
        );
    }

    if result.size == 0 {
        return format!(
            "terminal response from {}: seq={} time={} ms",
            ctx.target.address.ip(),
            result.seq,
            fmt_duration_ms_value(result.rtt)
        );
    }

    let ttl = result
        .ttl
        .map(|ttl| format!(" ttl={ttl}"))
        .unwrap_or_default();
    format!(
        "{} bytes from {}: seq={}{} time={} ms",
        result.size,
        ctx.target.address.ip(),
        result.seq,
        ttl,
        fmt_duration_ms_value(result.rtt)
    )
}

/// Returns the time since the previous reply and records `received_at` as the new previous reply.
pub(crate) fn inter_packet_interval(
    previous: &mut Option<Instant>,
    received_at: Instant,
) -> Option<Duration> {
    previous
        .replace(received_at)
        .map(|previous| received_at.saturating_duration_since(previous))
}

pub(crate) fn packet_loss_line(report: &PingReport, elapsed: Duration) -> String {
    format!(
        "{} packets transmitted, {} received, {:.0}% packet loss, time {}ms",
        report.sent(),
        report.received(),
        report.loss_pct(),
        elapsed.as_millis()
    )
}

pub(crate) fn rtt_line(report: &PingReport) -> Option<String> {
    Some(format!(
        "rtt min/avg/max/mdev = {}/{}/{}/{} ms",
        fmt_duration_ms_value(report.min()?),
        fmt_duration_ms_value(report.mean()?),
        fmt_duration_ms_value(report.max()?),
        fmt_duration_ms_value(report.std_dev()?),
    ))
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub(crate) fn jsonl_metadata(
    ctx: &OutputContext,
    interval: Duration,
    adaptive: bool,
    started_at_ms: u64,
) -> String {
    serde_json::to_string(&json!({
        "schema": "rup.ping",
        "version": 3,
        "record": "metadata",
        "started_at_ms": started_at_ms,
        "target": ctx.target.input,
        "address": ctx.target.address.ip(),
        "protocol": ctx.protocol.as_str(),
        "request_size_bytes": data_size(ctx),
        "packet_size_bytes": total_packet_size(ctx),
        "interval_ms": interval.as_millis(),
        "adaptive": adaptive,
    }))
    .expect("JSON values serialize")
}

pub(crate) fn jsonl_event(event: &PingEvent, elapsed: Duration, started_at_ms: u64) -> String {
    let timestamp_ms = absolute_timestamp_ms(started_at_ms, elapsed);
    let value = match event {
        PingEvent::Reply(result) => json!({
            "record": if result.size == 0 { "terminal_reply" } else { "reply" },
            "timestamp_ms": timestamp_ms,
            "seq": result.seq,
            "rtt_ms": duration_ms(result.rtt),
            "size_bytes": result.size,
            "ttl": result.ttl,
        }),
        PingEvent::Timeout { seq } => json!({
            "record": "timeout",
            "timestamp_ms": timestamp_ms,
            "seq": seq,
        }),
        PingEvent::ReorderOrLoss { seq } => json!({
            "record": "reorder_or_loss",
            "timestamp_ms": timestamp_ms,
            "seq": seq,
        }),
    };
    serde_json::to_string(&value).expect("JSON values serialize")
}

pub(crate) fn jsonl_summary(report: &PingReport, elapsed: Duration, started_at_ms: u64) -> String {
    serde_json::to_string(&json!({
        "record": "summary",
        "timestamp_ms": absolute_timestamp_ms(started_at_ms, elapsed),
        "sent": report.sent(),
        "received": report.received(),
        "loss_percent": report.loss_pct(),
        "rtt_min_ms": report.min().map(duration_ms),
        "rtt_mean_ms": report.mean().map(duration_ms),
        "rtt_median_ms": report.median().map(duration_ms),
        "rtt_max_ms": report.max().map(duration_ms),
        "rtt_std_dev_ms": report.std_dev().map(duration_ms),
    }))
    .expect("JSON values serialize")
}

fn absolute_timestamp_ms(started_at_ms: u64, elapsed: Duration) -> u64 {
    started_at_ms.saturating_add(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

pub(crate) fn unix_timestamp_ms() -> io::Result<u64> {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            io::Error::other(format!("system clock is before the Unix epoch: {error}"))
        })?
        .as_millis();
    u64::try_from(milliseconds)
        .map_err(|_| io::Error::other("system time does not fit into milliseconds"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_records_are_valid_and_versioned() {
        let ctx = OutputContext {
            target: ResolvedTarget {
                input: "example.test".to_owned(),
                address: "192.0.2.1:0".parse().unwrap(),
            },
            protocol: Protocol::Icmp,
            request_size: Some(PacketSize::new(56).unwrap()),
        };
        let reply = PingResult {
            seq: 7,
            rtt: Duration::from_micros(42),
            received_at: Instant::now(),
            size: 56,
            ttl: Some(64),
        };
        let report = PingReport::from_rtts(
            3,
            vec![Duration::from_micros(39), Duration::from_micros(42)],
        )
        .unwrap();

        let records = [
            jsonl_metadata(&ctx, Duration::from_millis(100), false, 1_700_000_000_000),
            jsonl_event(
                &PingEvent::Reply(reply),
                Duration::from_millis(150),
                1_700_000_000_000,
            ),
            jsonl_event(
                &PingEvent::Timeout { seq: 8 },
                Duration::from_millis(250),
                1_700_000_000_000,
            ),
            jsonl_event(
                &PingEvent::ReorderOrLoss { seq: 9 },
                Duration::from_millis(300),
                1_700_000_000_000,
            ),
            jsonl_summary(&report, Duration::from_millis(350), 1_700_000_000_000),
        ];
        let values = records
            .iter()
            .map(|record| serde_json::from_str::<serde_json::Value>(record).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(values[0]["schema"], "rup.ping");
        assert_eq!(values[0]["version"], 3);
        assert_eq!(values[0]["record"], "metadata");
        assert_eq!(values[0]["started_at_ms"], 1_700_000_000_000_u64);
        assert_eq!(values[1]["record"], "reply");
        assert_eq!(values[1]["timestamp_ms"], 1_700_000_000_150_u64);
        assert_eq!(values[1]["rtt_ms"], 0.042);
        assert_eq!(values[2]["record"], "timeout");
        assert_eq!(values[2]["timestamp_ms"], 1_700_000_000_250_u64);
        assert_eq!(values[3]["record"], "reorder_or_loss");
        assert_eq!(values[4]["record"], "summary");
        assert_eq!(values[4]["timestamp_ms"], 1_700_000_000_350_u64);
        assert!((values[4]["loss_percent"].as_f64().unwrap() - 100.0 / 3.0).abs() < 1e-12);
        assert!(values.iter().all(|value| value.get("elapsed_ms").is_none()));
        assert!(records.iter().all(|record| !record.contains('\n')));
    }

    #[test]
    fn jsonl_terminal_reply_has_distinct_record_type() {
        let event = PingEvent::Reply(PingResult {
            seq: 3,
            rtt: Duration::from_micros(75),
            received_at: Instant::now(),
            size: 0,
            ttl: None,
        });

        let value: serde_json::Value = serde_json::from_str(&jsonl_event(
            &event,
            Duration::from_millis(20),
            1_700_000_000_000,
        ))
        .unwrap();

        assert_eq!(value["record"], "terminal_reply");
        assert_eq!(value["timestamp_ms"], 1_700_000_000_020_u64);
        assert!(value.get("elapsed_ms").is_none());
    }

    #[test]
    fn header_line_matches_ping_shape() {
        let ctx = OutputContext {
            target: ResolvedTarget {
                input: "127.0.0.1".to_string(),
                address: "127.0.0.1:0".parse().unwrap(),
            },
            protocol: Protocol::Icmp,
            request_size: Some(PacketSize::new(56).unwrap()),
        };

        assert_eq!(
            header_line(&ctx),
            "PING 127.0.0.1 (127.0.0.1) 56(84) bytes of data."
        );
    }

    #[test]
    fn header_line_names_http_target_and_version() {
        let ctx = OutputContext {
            target: ResolvedTarget {
                input: "https://example.test".to_string(),
                address: "192.0.2.1:443".parse().unwrap(),
            },
            protocol: Protocol::Http2,
            request_size: None,
        };

        assert_eq!(
            header_line(&ctx),
            "PING https://example.test (192.0.2.1:443) via http2."
        );
    }

    #[test]
    fn reply_line_uses_seq_and_optional_ttl() {
        let ctx = OutputContext {
            target: ResolvedTarget {
                input: "127.0.0.1".to_string(),
                address: "127.0.0.1:0".parse().unwrap(),
            },
            protocol: Protocol::Icmp,
            request_size: None,
        };
        let result = PingResult {
            seq: 0,
            rtt: Duration::from_micros(42),
            received_at: Instant::now(),
            size: 64,
            ttl: Some(64),
        };

        assert_eq!(
            reply_line(&ctx, &result, None, None),
            "64 bytes from 127.0.0.1: seq=0 ttl=64 time=0.042 ms"
        );
    }

    #[test]
    fn reply_line_labels_terminal_network_response() {
        let ctx = OutputContext {
            target: ResolvedTarget {
                input: "127.0.0.1:5000".to_string(),
                address: "127.0.0.1:5000".parse().unwrap(),
            },
            protocol: Protocol::Udp,
            request_size: None,
        };
        let result = PingResult {
            seq: 4,
            rtt: Duration::from_micros(80),
            received_at: Instant::now(),
            size: 0,
            ttl: None,
        };

        assert_eq!(
            reply_line(&ctx, &result, None, None),
            "terminal response from 127.0.0.1: seq=4 time=0.080 ms"
        );
    }

    #[test]
    fn reply_line_uses_custom_format_when_present() {
        let ctx = OutputContext {
            target: ResolvedTarget {
                input: "example.test".to_string(),
                address: "192.0.2.1:0".parse().unwrap(),
            },
            protocol: Protocol::Icmp,
            request_size: None,
        };
        let result = PingResult {
            seq: 7,
            rtt: Duration::from_micros(42),
            received_at: Instant::now(),
            size: 64,
            ttl: Some(64),
        };
        let format = "{ip}: {seq} => {rtt}".parse().unwrap();

        assert_eq!(
            reply_line(&ctx, &result, Some(&format), None),
            "192.0.2.1: 7 => 0.042 ms"
        );
    }

    #[test]
    fn reply_line_renders_inter_packet_interval_from_custom_format() {
        let ctx = OutputContext {
            target: ResolvedTarget {
                input: "172.16.101.1".to_string(),
                address: "172.16.101.1:0".parse().unwrap(),
            },
            protocol: Protocol::Icmp,
            request_size: None,
        };
        let result = PingResult {
            seq: 100,
            rtt: Duration::from_millis(56),
            received_at: Instant::now(),
            size: 64,
            ttl: Some(64),
        };
        let format = r"{inter_packet}\n{seq} => {rtt}".parse().unwrap();

        assert_eq!(
            reply_line(
                &ctx,
                &result,
                Some(&format),
                Some(Duration::from_millis(1200))
            ),
            "1200.000 ms\n100 => 56.000 ms"
        );
    }

    #[test]
    fn default_reply_line_omits_inter_packet_interval() {
        let ctx = OutputContext {
            target: ResolvedTarget {
                input: "127.0.0.1".to_string(),
                address: "127.0.0.1:0".parse().unwrap(),
            },
            protocol: Protocol::Icmp,
            request_size: None,
        };
        let result = PingResult {
            seq: 1,
            rtt: Duration::from_micros(42),
            received_at: Instant::now(),
            size: 64,
            ttl: Some(64),
        };

        assert_eq!(
            reply_line(&ctx, &result, None, Some(Duration::from_millis(1000))),
            "64 bytes from 127.0.0.1: seq=1 ttl=64 time=0.042 ms"
        );
    }

    #[test]
    fn inter_packet_interval_measures_time_since_previous_reply() {
        let started = Instant::now();
        let mut previous = None;

        assert_eq!(inter_packet_interval(&mut previous, started), None);
        assert_eq!(
            inter_packet_interval(&mut previous, started + Duration::from_millis(459)),
            Some(Duration::from_millis(459))
        );
        assert_eq!(
            inter_packet_interval(&mut previous, started + Duration::from_millis(1659)),
            Some(Duration::from_millis(1200))
        );
    }

    #[test]
    fn inter_packet_interval_saturates_for_out_of_order_timestamps() {
        let started = Instant::now();
        let mut previous = Some(started + Duration::from_millis(5));

        assert_eq!(
            inter_packet_interval(&mut previous, started),
            Some(Duration::ZERO)
        );
        assert_eq!(previous, Some(started));
    }

    #[test]
    fn packet_loss_and_rtt_lines_match_ping_shape() {
        let report = PingReport::from_rtts(
            2,
            vec![Duration::from_micros(39), Duration::from_micros(42)],
        )
        .unwrap();

        assert_eq!(
            packet_loss_line(&report, Duration::from_millis(1041)),
            "2 packets transmitted, 2 received, 0% packet loss, time 1041ms"
        );
        assert_eq!(
            rtt_line(&report).unwrap(),
            "rtt min/avg/max/mdev = 0.039/0.041/0.042/0.002 ms"
        );
    }

    #[test]
    fn rtt_line_is_omitted_without_replies() {
        let report = PingReport::from_rtts(1, vec![]).unwrap();

        assert!(rtt_line(&report).is_none());
    }
}
