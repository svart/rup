use std::io;
use std::time::Duration;

use tokio::runtime;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::cli::CliParams::{PingerParams, ServerParams};
use pinger::{SendMode, StatEntry};

mod transport;
mod cli;
mod pinger;
mod statistics;

use transport::async_udp::UdpClientTransport;
use transport::transmitter;
use pinger::Request;

fn main() -> Result<(), io::Error> {
    let channel_cap: usize = 32;

    let cli_params = cli::get_cli_params();

    let rt = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    match cli_params {
        PingerParams(params) => {
            let (gen_txtr_send, gen_txtr_recv): (Sender<Request>, Receiver<Request>) =
                mpsc::channel(channel_cap);
            let (txtr_stat_send, txtr_stat_recv): (Sender<StatEntry>, Receiver<StatEntry>) =
                mpsc::channel(channel_cap);

            let (send_mode, txtr_gen) = if params.adaptive {
                let (txtr_gen_send, txtr_gen_recv): (Sender<()>, Receiver<()>) =
                    mpsc::channel(channel_cap);

                (SendMode::Adaptive(txtr_gen_recv), Some(txtr_gen_send))
            } else {
                (SendMode::Interval(params.interval), None)
            };

            rt.block_on(async {
                let transport = match params.protocol.as_str() {
                    // "tcp" => tokio::spawn(async_tcp::pinger_transport(
                    //     gen_txtr_recv,
                    //     txtr_stat_send,
                    //     params.local_address,
                    //     params.remote_address,
                    //     params.request_size,
                    //     params.response_size,
                    // )),
                    "udp" => UdpClientTransport::new(params.local_address, params.remote_address).await,
                    // "icmp" => tokio::spawn(async_icmp::pinger_transport(
                    //     gen_txtr_recv,
                    //     txtr_stat_send,
                    //     params.local_address,
                    //     params.remote_address,
                    //     params.request_size,
                    //     params.response_size,
                    // )),
                    _ => unreachable!(),
                };

                let transmitter = tokio::spawn(transmitter(transport, gen_txtr_recv, txtr_stat_send));

                let generator = tokio::spawn(pinger::generator(
                    gen_txtr_send,
                    send_mode,
                    params.ping_number,
                    params.run_time,
                    params.request_size,
                    params.response_size,
                ));
                let statista = tokio::spawn(statistics::statista(
                    txtr_stat_recv,
                    txtr_gen,
                    Duration::from_millis(params.wait_time),
                ));

                transmitter.await.unwrap();
                generator.await.unwrap();
                statista.await.unwrap();
            });
        }
        ServerParams(params) => {
            let server = match params.protocol.as_str() {
                "tcp" => rt.spawn(transport::async_tcp::server_transport(params.local_address)),
                "udp" => rt.spawn(transport::async_udp::server_transport(params.local_address)),
                "icmp" => panic!("there is no server for icmp"),
                _ => unreachable!(),
            };

            rt.block_on(async {
                server.await.unwrap();
            });
        }
    }

    Ok(())
}
