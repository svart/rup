use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::runtime;

mod cli;

use cli::CliParams::{PingerParams, ServerParams};
use rup::{PING_HDR_LEN, PacketSize, PingConfig, PingEvent, PingReport, PingResult, Protocol};

struct OutputContext {
    target: String,
    remote: SocketAddr,
    protocol: Protocol,
    request_size: Option<PacketSize>,
}

async fn resolve_remote_address(remote: &str, protocol: Protocol) -> io::Result<SocketAddr> {
    let addr = rup::ensure_port(remote, protocol)?;
    let mut addrs = tokio::net::lookup_host(&addr)
        .await
        .map_err(|e| io::Error::other(format!("failed to resolve '{remote}': {e}")))?;

    addrs
        .next()
        .ok_or_else(|| io::Error::other(format!("no addresses found for {remote}")))
}

fn fmt_duration_ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1000.0)
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
    let ip_header = if ctx.remote.is_ipv4() { 20 } else { 40 };
    let protocol_header = match ctx.protocol {
        Protocol::Icmp | Protocol::Udp => 8,
        Protocol::Tcp => 20,
    };
    data_size(ctx) + ip_header + protocol_header
}

fn header_line(ctx: &OutputContext) -> String {
    format!(
        "PING {} ({}) {}({}) bytes of data.",
        ctx.target,
        ctx.remote.ip(),
        data_size(ctx),
        total_packet_size(ctx)
    )
}

fn reply_line(ctx: &OutputContext, result: &PingResult) -> String {
    let ttl = result
        .ttl
        .map(|ttl| format!(" ttl={ttl}"))
        .unwrap_or_default();
    format!(
        "{} bytes from {}: seq={}{} time={}",
        result.size,
        ctx.remote.ip(),
        result.seq,
        ttl,
        fmt_duration_ms(result.rtt)
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

fn display_elapsed(
    elapsed: Duration,
    report: &PingReport,
    interval: u64,
    adaptive: bool,
    ping_number: Option<u64>,
) -> Duration {
    if !adaptive && report.sent > 0 && ping_number == Some(report.sent) {
        elapsed.saturating_sub(Duration::from_millis(interval))
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

fn print_event(ctx: &OutputContext, event: PingEvent) {
    match event {
        PingEvent::Reply(result) => {
            println!("{}", reply_line(ctx, &result));
        }
        PingEvent::Timeout { .. } | PingEvent::ReorderOrLoss { .. } => {}
    }
}

fn print_report(ctx: &OutputContext, report: &PingReport, elapsed: Duration) {
    println!("\n--- {} ping statistics ---", ctx.target);
    println!("{}", packet_loss_line(report, elapsed));
    if let Some(line) = rtt_line(report) {
        println!("{line}");
    }
}

fn main() {
    let cli_params = cli::get_cli_params();

    let rt = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .expect("failed to build runtime");

    match cli_params {
        PingerParams(params) => {
            rt.block_on(async {
                let remote_address =
                    match resolve_remote_address(&params.remote_address, params.protocol).await {
                        Ok(remote_address) => remote_address,
                        Err(e) => {
                            eprintln!("{e}");
                            return;
                        }
                    };

                let ctx = OutputContext {
                    target: params.remote_address.clone(),
                    remote: remote_address,
                    protocol: params.protocol,
                    request_size: params.request_size,
                };

                let config = PingConfig {
                    remote: remote_address,
                    local: params.local_address,
                    protocol: params.protocol,
                    interval: Duration::from_millis(params.interval),
                    adaptive: params.adaptive,
                    wait_time: Duration::from_millis(params.wait_time),
                    request_size: params.request_size,
                    response_size: params.response_size,
                    tos: params.tos,
                    ping_number: params.ping_number,
                    run_time: params.run_time,
                };

                println!("{}", header_line(&ctx));
                let started = Instant::now();
                match rup::start_ping_session(config).await {
                    Ok(mut session) => {
                        while let Some(event) = session.next().await {
                            print_event(&ctx, event);
                        }

                        match session.report().await {
                            Ok(report) => {
                                let elapsed = display_elapsed(
                                    started.elapsed(),
                                    &report,
                                    params.interval,
                                    params.adaptive,
                                    params.ping_number,
                                );
                                print_report(&ctx, &report, elapsed);
                            }
                            Err(e) => eprintln!("{e}"),
                        }
                    }
                    Err(e) => eprintln!("{e}"),
                }
            });
        }
        ServerParams(params) => {
            rt.block_on(async {
                if let Err(e) = rup::run_server(params.protocol, params.local_address).await {
                    eprintln!("{e}");
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_line_matches_ping_shape() {
        let ctx = OutputContext {
            target: "127.0.0.1".to_string(),
            remote: "127.0.0.1:0".parse().unwrap(),
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
            target: "127.0.0.1".to_string(),
            remote: "127.0.0.1:0".parse().unwrap(),
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
            display_elapsed(Duration::from_millis(2004), &report, 1000, false, Some(2)),
            Duration::from_millis(1004)
        );
        assert_eq!(
            display_elapsed(Duration::from_millis(2004), &report, 1000, true, Some(2)),
            Duration::from_millis(2004)
        );
    }

    #[tokio::test]
    async fn resolve_remote_address_adds_zero_port_for_icmp() {
        let addr = resolve_remote_address("127.0.0.1", Protocol::Icmp)
            .await
            .unwrap();

        assert_eq!(addr, "127.0.0.1:0".parse().unwrap());
    }

    #[tokio::test]
    async fn resolve_remote_address_rejects_udp_without_port() {
        let err = resolve_remote_address("127.0.0.1", Protocol::Udp)
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
