use std::time::Duration;

use tokio::runtime;

mod cli;

use cli::CliParams::{PingerParams, ServerParams};
use rup::PingConfig;

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

                if let Err(e) = rup::run_ping_session_with_output(config).await {
                    eprintln!("{e}");
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
