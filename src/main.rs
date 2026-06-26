use std::time::Duration;

use tokio::runtime;

mod cli;

use cli::CliParams::{PingerParams, ServerParams};
use rup::{PingConfig, PingEvent, PingReport};

fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 1.0 {
        format!("{secs:.3} s")
    } else if secs >= 0.001 {
        format!("{:.3} ms", secs * 1000.0)
    } else {
        format!("{:.3} µs", secs * 1_000_000.0)
    }
}

fn print_event(event: PingEvent) {
    match event {
        PingEvent::Reply(result) => {
            println!("seq={} time={}", result.seq, fmt_duration(result.rtt))
        }
        PingEvent::Timeout { seq } => println!("seq={seq} timeout"),
        PingEvent::ReorderOrLoss { seq } => println!("seq={seq} reorder or loss"),
    }
}

fn print_report(report: &PingReport) {
    if report.rtts.is_empty() {
        println!("no statistics collected");
        return;
    }

    println!(
        "\n--- statistics ---\n\
         {sr} requests sent, {rc} received, {loss:.0}% loss\n\
         min/med/avg/max = {mi} / {me} / {av} / {ma}\n\
         std_dev = {sd}",
        sr = report.sent,
        rc = report.received,
        loss = report.loss_pct(),
        mi = fmt_duration(report.min().unwrap_or_default()),
        me = fmt_duration(report.median().unwrap_or_default()),
        av = fmt_duration(report.mean().unwrap_or_default()),
        ma = fmt_duration(report.max().unwrap_or_default()),
        sd = fmt_duration(report.std_dev().unwrap_or_default()),
    );
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
                let config = PingConfig {
                    remote: params.remote_address,
                    local: params.local_address,
                    protocol: params.protocol,
                    interval: params.interval,
                    adaptive: params.adaptive,
                    wait_time: Duration::from_millis(params.wait_time),
                    request_size: params.request_size,
                    response_size: params.response_size,
                    tos: params.tos,
                    ping_number: params.ping_number,
                    run_time: params.run_time,
                };

                match rup::start_ping_session(config).await {
                    Ok(mut session) => {
                        while let Some(event) = session.next().await {
                            print_event(event);
                        }

                        match session.report().await {
                            Ok(report) => print_report(&report),
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
    fn duration_format_uses_expected_units() {
        assert_eq!(fmt_duration(Duration::from_micros(50)), "50.000 µs");
        assert_eq!(fmt_duration(Duration::from_millis(5)), "5.000 ms");
        assert_eq!(fmt_duration(Duration::from_secs(2)), "2.000 s");
    }
}
