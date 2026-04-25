use std::time::Duration;

use tokio::runtime;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;

mod cli;

use cli::CliParams::{PingerParams, ServerParams};
use rup::pinger::{Request, SendMode, StatEntry};
#[cfg(test)]
use rup::pinger::Response;
use rup::transport::async_icmp::IcmpClientTransport;
use rup::transport::async_tcp::TcpClientTransport;
use rup::transport::async_udp::UdpClientTransport;
use rup::transport::Transport;
use rup::transport::{receiver, transmitter};
use rup::ensure_port;

fn spawn_tasks<T: Transport + Clone + Send + 'static>(
    transport: T,
    gen_txtr_recv: Receiver<Request>,
    txtr_stat_send: Sender<StatEntry>,
) -> (JoinHandle<()>, JoinHandle<()>) {
    let t2 = transport.clone();
    let tx = tokio::spawn(transmitter(t2, gen_txtr_recv, txtr_stat_send.clone()));
    let rx = tokio::spawn(receiver(transport, txtr_stat_send));
    (tx, rx)
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
            let channel_cap = 1024;
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
                let addr = ensure_port(&params.remote_address, &params.protocol);
                let remote_addr = match tokio::net::lookup_host(&addr).await {
                    Ok(mut addrs) => match addrs.next() {
                        Some(a) => a,
                        None => {
                            eprintln!("no addresses found for {}", params.remote_address);
                            return;
                        }
                    },
                    Err(e) => {
                        eprintln!("failed to resolve '{}': {}", params.remote_address, e);
                        return;
                    }
                };

                let (mut tx_handle, mut rx_handle) = match params.protocol.as_str() {
                    "udp" => {
                        let transport = match UdpClientTransport::new(
                            params.local_address,
                            remote_addr,
                        )
                        .await
                        {
                            Ok(t) => t,
                            Err(e) => {
                                eprintln!("UDP transport failed: {e}");
                                return;
                            }
                        };
                        spawn_tasks(transport, gen_txtr_recv, txtr_stat_send)
                    }
                    "tcp" => {
                        let sock = if remote_addr.is_ipv4() {
                            match tokio::net::TcpSocket::new_v4() {
                                Ok(s) => s,
                                Err(e) => {
                                    eprintln!("TCP socket creation failed: {e}");
                                    return;
                                }
                            }
                        } else {
                            match tokio::net::TcpSocket::new_v6() {
                                Ok(s) => s,
                                Err(e) => {
                                    eprintln!("TCP socket creation failed: {e}");
                                    return;
                                }
                            }
                        };
                        if let Err(e) = sock.bind(params.local_address) {
                            eprintln!("TCP bind failed: {e}");
                            return;
                        }
                        let stream = match sock.connect(remote_addr).await {
                            Ok(s) => s,
                            Err(e) => {
                                eprintln!("TCP connect failed: {e}");
                                return;
                            }
                        };
                        let transport = TcpClientTransport::new(stream);
                        spawn_tasks(transport, gen_txtr_recv, txtr_stat_send)
                    }
                    "icmp" => {
                        let transport = match IcmpClientTransport::new(
                            params.local_address,
                            remote_addr,
                        )
                        .await
                        {
                            Ok(t) => t,
                            Err(e) => {
                                eprintln!("ICMP transport failed: {e}");
                                return;
                            }
                        };
                        spawn_tasks(transport, gen_txtr_recv, txtr_stat_send)
                    }
                    _ => {
                        eprintln!("unknown protocol: {}", params.protocol);
                        return;
                    }
                };

                let generator = tokio::spawn(rup::pinger::generator(
                    gen_txtr_send,
                    send_mode,
                    params.ping_number,
                    params.run_time,
                    params.request_size,
                    params.response_size,
                ));
                let statista = tokio::spawn(rup::statistics::statista(
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
                let _ = generator.await;
                let _ = statista.await;
            });
        }
        ServerParams(params) => {
            rt.block_on(async {
                let server = match params.protocol.as_str() {
                    "tcp" => tokio::spawn(rup::transport::async_tcp::server_transport(
                        params.local_address,
                    )),
                    "udp" => tokio::spawn(rup::transport::async_udp::server_transport(
                        params.local_address,
                    )),
                    "icmp" => {
                        eprintln!("there is no server for ICMP");
                        return;
                    }
                    _ => {
                        eprintln!("unknown protocol: {}", params.protocol);
                        return;
                    }
                };

                let _ = server.await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::time::Instant;
    use rup::transport::Transport;

    struct NoopTransport;

    impl Clone for NoopTransport {
        fn clone(&self) -> Self { NoopTransport }
    }

    impl Transport for NoopTransport {
        async fn send(&self, _: &Request) -> io::Result<Instant> {
            Ok(Instant::now())
        }
        async fn recv(&self) -> io::Result<Response> {
            Err(io::Error::new(io::ErrorKind::Other, "noop"))
        }
    }

    #[tokio::test]
    async fn spawn_tasks_wires_transmitter_and_receiver() {
        let (req_tx, req_rx) = mpsc::channel(8);
        let (stat_tx, _stat_rx) = mpsc::channel(8);

        let (tx_h, rx_h) = spawn_tasks(NoopTransport, req_rx, stat_tx);

        req_tx.send(Request { id: 0, request_size: None, response_size: None }).await.unwrap();
        drop(req_tx);

        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), tx_h).await;
        rx_h.abort();
    }
}
