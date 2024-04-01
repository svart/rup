use std::io;
use std::time::Duration;

use tokio::runtime;
use tokio::sync::mpsc::{self, Receiver, Sender};

use pinger::{PingReqResp, SendMode};
use crate::cli::CliParams::{PingerParams, ServerParams};

mod async_tcp;
mod async_udp;
mod pinger;
mod statistics;
mod cli;

fn main() -> Result<(), io::Error> {
    let channel_cap: usize = 32;

    let cli_params = cli::get_cli_params();

    let rt = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    match cli_params {
        PingerParams(params) => {
            let (gen_txtr_send, gen_txtr_recv): (Sender<PingReqResp>, Receiver<PingReqResp>) =
                mpsc::channel(channel_cap);
            let (txtr_stat_send, txtr_stat_recv): (Sender<PingReqResp>, Receiver<PingReqResp>) =
                mpsc::channel(channel_cap);

            let (send_mode, txtr_gen) = if params.adaptive {
                let (txtr_gen_send, txtr_gen_recv): (Sender<u8>, Receiver<u8>) =
                    mpsc::channel(channel_cap);

                (SendMode::Adaptive(txtr_gen_recv), Some(txtr_gen_send))
            } else {
                (SendMode::Interval(params.interval), None)
            };

            let pinger = match params.protocol.as_str() {
                "tcp" => rt.spawn(async_tcp::pinger_transport(
                    gen_txtr_recv,
                    txtr_stat_send,
                    params.local_address,
                    params.remote_address,
                    params.request_size,
                    params.response_size,
                )),
                "udp" => rt.spawn(async_udp::pinger_transport(
                    gen_txtr_recv,
                    txtr_stat_send,
                    params.local_address,
                    params.remote_address,
                    params.request_size,
                    params.response_size,
                )),
                "icmp" => unimplemented!(),
                _ => unreachable!(),
            };

            let generator = rt.spawn(
                pinger::generator(gen_txtr_send, send_mode, params.ping_number)
            );
            let statista = rt.spawn(statistics::statista(
                txtr_stat_recv,
                txtr_gen,
                Duration::from_millis(params.wait_time),
            ));

            rt.block_on(async {
                pinger.await.unwrap();
                generator.await.unwrap();
                statista.await.unwrap();
            });
        }
        ServerParams(params) => {
            let server = match params.protocol.as_str() {
                "tcp" => rt.spawn(async_tcp::server_transport(params.local_address)),
                "udp" => rt.spawn(async_udp::server_transport(params.local_address)),
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
