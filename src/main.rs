use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::runtime;

mod cli;

use cli::CliParams::{PingerParams, ServerParams};
use rup::{PING_HDR_LEN, PacketSize, PingConfig, PingEvent, PingReport, PingResult, Protocol};

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
    let ttl = result
        .ttl
        .map(|ttl| format!(" ttl={ttl}"))
        .unwrap_or_default();
    format!(
        "{} bytes from {}: seq={}{} time={}",
        result.size,
        ctx.target.address.ip(),
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

fn print_event(ctx: &OutputContext, event: PingEvent) {
    if let PingEvent::Reply(result) = event {
        println!("{}", reply_line(ctx, &result));
    }
}

fn print_report(ctx: &OutputContext, report: &PingReport, elapsed: Duration) {
    println!("\n--- {} ping statistics ---", ctx.target.input);
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

    println!("{}", header_line(&ctx));
    let started = Instant::now();
    match rup::start_ping_session(config).await {
        Ok(mut session) => {
            while let Some(event) = session.next().await {
                print_event(&ctx, event);
            }

            match session.report().await {
                Ok(report) => {
                    let elapsed =
                        display_elapsed(started.elapsed(), &report, DisplayTiming::from(&params));
                    print_report(&ctx, &report, elapsed);
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
