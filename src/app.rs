use std::io;
use std::io::IsTerminal;
use std::time::Instant;

use tokio::runtime;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::prelude::*;

use crate::cli::CliParams::{PingerParams, ServerParams};
use crate::cli::{self, OutputFormat};
use crate::output::{
    OutputContext, ResolvedTarget, header_line, inter_packet_interval, jsonl_event, jsonl_metadata,
    jsonl_summary, packet_loss_line, reply_line, rtt_line, unix_timestamp_ms,
};
use rup::{HttpTarget, HttpVersion, PingEvent, Pinger, Protocol, SessionConfig, Target};

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

/// Logs `rup` warnings to stderr, or every `rup` event with `--verbose`.
/// Dependency events (for example h2 frame traces) are filtered out.
fn init_tracing(verbose: bool) {
    let level = if verbose {
        tracing::Level::TRACE
    } else {
        tracing::Level::WARN
    };
    let _ = tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(std::io::stderr().is_terminal()),
        )
        .with(Targets::new().with_target("rup", level))
        .try_init();
}

pub(crate) fn run() -> std::process::ExitCode {
    let cli_params = cli::get_cli_params();
    init_tracing(match &cli_params {
        PingerParams(params) => params.verbose,
        ServerParams(params) => params.verbose,
    });

    let rt = match runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(error) => {
            eprintln!("failed to build runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let result = rt.block_on(async {
        match cli_params {
            PingerParams(params) => run_client(params).await,
            ServerParams(params) => rup::run_server(params.protocol, params.local_address).await,
        }
    });

    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run_client(params: cli::PingerParams) -> io::Result<()> {
    let target = resolve_remote_target(params.remote_address.clone(), params.protocol).await?;

    let ctx = OutputContext {
        target: target.clone(),
        protocol: params.protocol,
        request_size: params.request_size,
    };

    let remote = target.address;
    let url = || {
        params
            .remote_address
            .parse::<HttpTarget>()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    };
    let ping_target = match params.protocol {
        Protocol::Udp => Target::Udp(remote),
        Protocol::Tcp => Target::Tcp(remote),
        Protocol::Icmp => Target::Icmp(remote),
        Protocol::Http1 => Target::Http {
            version: HttpVersion::Http1,
            remote,
            url: url()?,
        },
        Protocol::Http2 => Target::Http {
            version: HttpVersion::Http2,
            remote,
            url: url()?,
        },
    };

    let mut session_config = SessionConfig::default()
        .interval(params.interval)
        .timeout(params.wait_time);
    if params.adaptive {
        session_config = session_config.adaptive();
    }
    if let Some(size) = params.request_size {
        session_config = session_config.request_size(size);
    }
    if let Some(size) = params.response_size {
        session_config = session_config.response_size(size);
    }
    if let Some(count) = params.ping_number {
        session_config = session_config.count(count);
    }
    if let Some(run_time) = params.run_time {
        session_config = session_config.run_time(run_time);
    }

    let mut config = Pinger::new(ping_target).session(session_config);
    if let Some(local_address) = params.local_address {
        config = config.local(local_address);
    }
    if let Some(traffic_class) = params.tos {
        config = config.traffic_class(traffic_class);
    }

    let started_at_ms = unix_timestamp_ms()?;
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
    let mut session = config.start().await?;
    let mut interrupted = false;
    let mut previous_reply_at = None;
    loop {
        let event = if interrupted {
            session.next().await
        } else {
            tokio::select! {
                event = session.next() => event,
                signal = tokio::signal::ctrl_c() => {
                    if let Err(e) = signal {
                        eprintln!("failed to listen for Ctrl+C: {e}");
                    }
                    session.stop();
                    interrupted = true;
                    continue;
                }
            }
        };
        let Some(event) = event else {
            break;
        };

        match params.output {
            OutputFormat::Human => {
                if let PingEvent::Reply(result) = event {
                    let inter_packet =
                        inter_packet_interval(&mut previous_reply_at, result.received_at);
                    println!(
                        "{}",
                        reply_line(&ctx, &result, params.format.as_ref(), inter_packet)
                    );
                }
            }
            OutputFormat::Jsonl => {
                println!("{}", jsonl_event(&event, started.elapsed(), started_at_ms));
            }
        }
    }

    let report = session.report().await?;
    let elapsed = started.elapsed();
    match params.output {
        OutputFormat::Human => {
            println!("\n--- {} ping statistics ---", ctx.target.input);
            println!("{}", packet_loss_line(&report, elapsed));
            if let Some(line) = rtt_line(&report) {
                println!("{line}");
            }
        }
        OutputFormat::Jsonl => println!("{}", jsonl_summary(&report, elapsed, started_at_ms)),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn resolve_remote_address_uses_http_url_default_port() {
        let target = resolve_remote_target("https://127.0.0.1/path".to_string(), Protocol::Http1)
            .await
            .unwrap();

        assert_eq!(target.address, "127.0.0.1:443".parse().unwrap());
        assert_eq!(target.input, "https://127.0.0.1/path");
    }
}
