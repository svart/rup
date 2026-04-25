use std::time::Duration;

use tokio::runtime;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;

use crate::cli::CliParams::{PingerParams, ServerParams};
use pinger::{Request, SendMode, StatEntry};
#[cfg(test)]
use pinger::Response;

mod transport;
mod cli;
mod pinger;
mod statistics;

use transport::async_icmp::IcmpClientTransport;
use transport::async_tcp::TcpClientTransport;
use transport::async_udp::UdpClientTransport;
use transport::Transport;
use transport::{receiver, transmitter};

fn has_port(addr: &str) -> bool {
    if addr.starts_with('[') {
        let after_bracket = addr.split(']').nth(1).unwrap_or("");
        after_bracket.starts_with(':')
    } else {
        let last_colon = addr.rfind(':');
        match last_colon {
            Some(i) => {
                let after = &addr[i + 1..];
                !after.is_empty() && after.chars().all(|c| c.is_ascii_digit())
            }
            None => false,
        }
    }
}

fn ensure_port(addr: &str, protocol: &str) -> String {
    if has_port(addr) {
        return addr.to_string();
    }
    if protocol == "icmp" {
        return format!("{addr}:0");
    }
    eprintln!("error: {protocol} requires a port (e.g. {addr}:PORT)");
    std::process::exit(1);
}

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
                let _ = generator.await;
                let _ = statista.await;
            });
        }
        ServerParams(params) => {
            rt.block_on(async {
                let server = match params.protocol.as_str() {
                    "tcp" => tokio::spawn(transport::async_tcp::server_transport(
                        params.local_address,
                    )),
                    "udp" => tokio::spawn(transport::async_udp::server_transport(
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
    use transport::Transport;

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

    #[test]
    fn has_port_detects_v4_with_port() {
        assert!(has_port("127.0.0.1:5000"));
    }

    #[test]
    fn has_port_detects_v4_without_port() {
        assert!(!has_port("127.0.0.1"));
    }

    #[test]
    fn has_port_detects_v6_with_port() {
        assert!(has_port("[::1]:5000"));
    }

    #[test]
    fn has_port_detects_v6_without_port() {
        assert!(!has_port("[::1]"));
    }

    #[test]
    fn has_port_detects_hostname_with_port() {
        assert!(has_port("localhost:8080"));
    }

    #[test]
    fn has_port_detects_hostname_without_port() {
        assert!(!has_port("localhost"));
    }

    #[test]
    fn has_port_empty_after_colon() {
        assert!(!has_port("127.0.0.1:"));
    }

    #[test]
    fn has_port_non_numeric_after_colon() {
        assert!(!has_port("127.0.0.1:abc"));
    }

    #[test]
    fn has_port_multiple_colons_ipv6() {
        assert!(has_port("[2001:db8::1]:8080"));
    }

    #[test]
    fn has_port_multiple_colons_no_port() {
        assert!(!has_port("[2001:db8::1]"));
    }

    #[test]
    fn has_port_empty_string() {
        assert!(!has_port(""));
    }

    #[test]
    fn has_port_just_port_number() {
        assert!(has_port(":5000"));
    }

    #[test]
    fn ensure_port_keeps_existing_v4() {
        assert_eq!(ensure_port("10.0.0.1:9999", "udp"), "10.0.0.1:9999");
    }

    #[test]
    fn ensure_port_keeps_existing_v6() {
        assert_eq!(ensure_port("[::1]:443", "tcp"), "[::1]:443");
    }

    #[test]
    fn ensure_port_adds_zero_for_icmp_v4() {
        assert_eq!(ensure_port("192.168.1.1", "icmp"), "192.168.1.1:0");
    }

    #[test]
    fn ensure_port_adds_zero_for_icmp_v6() {
        assert_eq!(ensure_port("[::1]", "icmp"), "[::1]:0");
    }

    #[test]
    fn ensure_port_adds_zero_for_icmp_hostname() {
        assert_eq!(ensure_port("localhost", "icmp"), "localhost:0");
    }
}
