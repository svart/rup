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

use transport::async_icmp::IcmpClientTransport;
use transport::async_tcp::TcpClientTransport;
use transport::async_udp::UdpClientTransport;
use transport::{transmitter, receiver};
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
                let (mut tx_handle, mut rx_handle) = match params.protocol.as_str() {
                    "udp" => {
                        let transport =
                            UdpClientTransport::new(params.local_address, params.remote_address)
                                .await;
                        let t2 = transport.clone();
                        let tx = tokio::spawn(transmitter(t2, gen_txtr_recv, txtr_stat_send.clone()));
                        let rx = tokio::spawn(receiver(transport, txtr_stat_send));
                        (tx, rx)
                    }
                    "tcp" => {
                        let sock = tokio::net::TcpSocket::new_v4().unwrap();
                        sock.bind(params.local_address).expect("TCP: bind failed");
                        let stream = sock
                            .connect(params.remote_address)
                            .await
                            .expect("TCP: connect failed");
                        let transport = TcpClientTransport::new(stream);
                        let tx = tokio::spawn(transmitter(
                            transport.clone(),
                            gen_txtr_recv,
                            txtr_stat_send.clone(),
                        ));
                        let rx =
                            tokio::spawn(receiver(transport, txtr_stat_send));
                        (tx, rx)
                    }
                    "icmp" => {
                        let transport = IcmpClientTransport::new(
                            params.local_address,
                            params.remote_address,
                        )
                        .await;
                        let tx = tokio::spawn(transmitter(
                            transport.clone(),
                            gen_txtr_recv,
                            txtr_stat_send.clone(),
                        ));
                        let rx = tokio::spawn(receiver(transport, txtr_stat_send));
                        (tx, rx)
                    }
                    _ => unreachable!(),
                };

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

                tokio::select! {
                    _ = &mut tx_handle => {
                        rx_handle.abort();
                    }
                    _ = &mut rx_handle => {}
                }

                drop(tx_handle);
                drop(rx_handle);
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
