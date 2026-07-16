use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::runtime;

mod cli;

use cli::CliParams::{PingerParams, ServerParams};
use cli::OutputFormat;
use rup::{PING_HDR_LEN, PacketSize, PingConfig, PingEvent, PingReport, PingResult, Protocol};
use serde_json::json;

#[derive(Clone, Debug)]
struct ResolvedTarget {
    input: String,
    address: SocketAddr,
}

struct OutputContext {
    target: ResolvedTarget,
    protocol: Protocol,
    request_size: Option<PacketSize>,
}

#[derive(Clone, Copy)]
struct DisplayTiming {
    interval: Duration,
    adaptive: bool,
    ping_number: Option<u64>,
}

impl From<&cli::PingerParams> for DisplayTiming {
    fn from(params: &cli::PingerParams) -> Self {
        Self {
            interval: params.interval,
            adaptive: params.adaptive,
            ping_number: params.ping_number,
        }
    }
}

async fn resolve_remote_target(remote: String, protocol: Protocol) -> io::Result<ResolvedTarget> {
    let addr = rup::ensure_port(&remote, protocol)?;
    let mut addrs = tokio::net::lookup_host(&addr)
        .await
        .map_err(|e| io::Error::other(format!("failed to resolve '{remote}': {e}")))?;

    let address = addrs
        .next()
        .ok_or_else(|| io::Error::other(format!("no addresses found for {remote}")))?;

    Ok(ResolvedTarget {
        input: remote,
        address,
    })
}

fn fmt_duration_ms_value(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64() * 1000.0)
}

fn data_size(ctx: &OutputContext) -> usize {
    ctx.request_size
        .map(PacketSize::get)
        .unwrap_or(PING_HDR_LEN as u16) as usize
}

fn total_packet_size(ctx: &OutputContext) -> usize {
    let ip_header = if ctx.target.address.is_ipv4() { 20 } else { 40 };
    let protocol_header = match ctx.protocol {
        Protocol::Icmp | Protocol::Udp => 8,
        Protocol::Tcp => 20,
    };
    data_size(ctx) + ip_header + protocol_header
}

fn header_line(ctx: &OutputContext) -> String {
    format!(
        "PING {} ({}) {}({}) bytes of data.",
        ctx.target.input,
        ctx.target.address.ip(),
        data_size(ctx),
        total_packet_size(ctx)
    )
}

fn reply_line(ctx: &OutputContext, result: &PingResult) -> String {
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

fn packet_loss_line(report: &PingReport, elapsed: Duration) -> String {
    format!(
        "{} packets transmitted, {} received, {:.0}% packet loss, time {}ms",
        report.sent,
        report.received,
        report.loss_pct(),
        elapsed.as_millis()
    )
}

fn display_elapsed(elapsed: Duration, report: &PingReport, timing: DisplayTiming) -> Duration {
    if !timing.adaptive && report.sent > 0 && timing.ping_number == Some(report.sent) {
        elapsed.saturating_sub(timing.interval)
    } else {
        elapsed
    }
}

fn rtt_line(report: &PingReport) -> Option<String> {
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

fn jsonl_metadata(
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

fn jsonl_event(event: &PingEvent, elapsed: Duration, started_at_ms: u64) -> String {
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

fn jsonl_summary(report: &PingReport, elapsed: Duration, started_at_ms: u64) -> String {
    serde_json::to_string(&json!({
        "record": "summary",
        "timestamp_ms": absolute_timestamp_ms(started_at_ms, elapsed),
        "sent": report.sent,
        "received": report.received,
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

fn unix_timestamp_ms() -> u64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_millis();
    u64::try_from(milliseconds).expect("system time does not fit into milliseconds")
}

fn main() {
    let cli_params = cli::get_cli_params();

    let rt = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("failed to build runtime");

    rt.block_on(async {
        match cli_params {
            PingerParams(params) => run_client(params).await,
            ServerParams(params) => {
                if let Err(e) = rup::run_server(params.protocol, params.local_address).await {
                    eprintln!("{e}");
                }
            }
        }
    });
}

async fn run_client(params: cli::PingerParams) {
    let target = match resolve_remote_target(params.remote_address.clone(), params.protocol).await {
        Ok(target) => target,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };

    let ctx = OutputContext {
        target: target.clone(),
        protocol: params.protocol,
        request_size: params.request_size,
    };

    let config = PingConfig {
        remote: target.address,
        local: params.local_address,
        protocol: params.protocol,
        interval: params.interval,
        adaptive: params.adaptive,
        wait_time: params.wait_time,
        request_size: params.request_size,
        response_size: params.response_size,
        tos: params.tos,
        ping_number: params.ping_number,
        run_time: params.run_time,
    };

    let started_at_ms = unix_timestamp_ms();
    let started = Instant::now();
    match params.output {
        OutputFormat::Human => println!("{}", header_line(&ctx)),
        OutputFormat::Jsonl => {
            println!(
                "{}",
                jsonl_metadata(&ctx, params.interval, params.adaptive, started_at_ms)
            )
        }
    }
    match rup::start_ping_session(config).await {
        Ok(mut session) => {
            while let Some(event) = session.next().await {
                match params.output {
                    OutputFormat::Human => {
                        if let PingEvent::Reply(result) = event {
                            println!("{}", reply_line(&ctx, &result));
                        }
                    }
                    OutputFormat::Jsonl => {
                        println!("{}", jsonl_event(&event, started.elapsed(), started_at_ms));
                    }
                }
            }

            match session.report().await {
                Ok(report) => {
                    let elapsed =
                        display_elapsed(started.elapsed(), &report, DisplayTiming::from(&params));
                    match params.output {
                        OutputFormat::Human => {
                            println!("\n--- {} ping statistics ---", ctx.target.input);
                            println!("{}", packet_loss_line(&report, elapsed));
                            if let Some(line) = rtt_line(&report) {
                                println!("{line}");
                            }
                        }
                        OutputFormat::Jsonl => {
                            println!("{}", jsonl_summary(&report, elapsed, started_at_ms))
                        }
                    }
                }
                Err(e) => eprintln!("{e}"),
            }
        }
        Err(e) => eprintln!("{e}"),
    }
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
            size: 56,
            ttl: Some(64),
        };
        let report = PingReport {
            rtts: vec![Duration::from_micros(39), Duration::from_micros(42)],
            sent: 3,
            received: 2,
        };

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
            size: 64,
            ttl: Some(64),
        };

        assert_eq!(
            reply_line(&ctx, &result),
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
            size: 0,
            ttl: None,
        };

        assert_eq!(
            reply_line(&ctx, &result),
            "terminal response from 127.0.0.1: seq=4 time=0.080 ms"
        );
    }

    #[test]
    fn packet_loss_and_rtt_lines_match_ping_shape() {
        let report = PingReport {
            rtts: vec![Duration::from_micros(39), Duration::from_micros(42)],
            sent: 2,
            received: 2,
        };

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
    fn display_elapsed_removes_final_interval_for_completed_fixed_count() {
        let report = PingReport {
            rtts: vec![Duration::from_millis(1), Duration::from_millis(1)],
            sent: 2,
            received: 2,
        };

        assert_eq!(
            display_elapsed(
                Duration::from_millis(2004),
                &report,
                DisplayTiming {
                    interval: Duration::from_millis(1000),
                    adaptive: false,
                    ping_number: Some(2),
                }
            ),
            Duration::from_millis(1004)
        );
        assert_eq!(
            display_elapsed(
                Duration::from_millis(2004),
                &report,
                DisplayTiming {
                    interval: Duration::from_millis(1000),
                    adaptive: true,
                    ping_number: Some(2),
                }
            ),
            Duration::from_millis(2004)
        );
    }

    #[tokio::test]
    async fn resolve_remote_address_adds_zero_port_for_icmp() {
        let target = resolve_remote_target("127.0.0.1".to_string(), Protocol::Icmp)
            .await
            .unwrap();

        assert_eq!(target.address, "127.0.0.1:0".parse().unwrap());
        assert_eq!(target.input, "127.0.0.1");
    }

    #[tokio::test]
    async fn resolve_remote_address_rejects_udp_without_port() {
        let err = resolve_remote_target("127.0.0.1".to_string(), Protocol::Udp)
            .await
            .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rtt_line_is_omitted_without_replies() {
        let report = PingReport {
            rtts: vec![],
            sent: 1,
            received: 0,
        };

        assert!(rtt_line(&report).is_none());
    }
}
