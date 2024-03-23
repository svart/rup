use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::mpsc::{self, Receiver, Sender};

use clap::{Arg, ArgAction, Command};
use tokio::runtime;

mod async_tcp;
mod async_udp;
mod pinger;
mod statistics;

use pinger::{PingReqResp, SendMode, PING_HDR_LEN};

fn cli() -> Command {
    Command::new("rup")
        .about("rup universal pinger")
        .subcommand(
            Command::new("client")
                .about("Send requests to the remote side and measure RTT")
                .arg(
                    Arg::new("remote-address")
                        .help("Were to send echo requests")
                        .action(ArgAction::Set)
                        .required(true)
                        .value_parser(clap::value_parser!(SocketAddr)),
                )
                .arg(
                    Arg::new("local-address")
                        .long("local-address")
                        .help("Set local address to bind to")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(SocketAddr))
                        .default_value("0.0.0.0:0"),
                )
                .arg(
                    Arg::new("interval")
                        .long("interval")
                        .short('i')
                        .help("Set interval in ms to send echo requests")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u64).range(1..))
                        .default_value("1000")
                        .conflicts_with("adaptive-interval"),
                )
                .arg(
                    Arg::new("adaptive-interval")
                        .long("adaptive-interval")
                        .short('A')
                        .help("Generate new request just after reception of responce")
                        .action(ArgAction::SetTrue)
                        .conflicts_with("interval"),
                )
                .arg(
                    Arg::new("wait-time")
                        .long("wait-time")
                        .short('W')
                        .help("Time to wait for responce im ms")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u64).range(1..))
                        .default_value("1000"),
                )
                .arg(
                    Arg::new("req-size")
                        .long("request-size")
                        .help("Size of echo request")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u16).range(PING_HDR_LEN as i64..)),
                )
                .arg(
                    Arg::new("resp-size")
                        .long("response-size")
                        .help("Size of echo response")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u16).range(PING_HDR_LEN as i64..)),
                )
                .arg(
                    Arg::new("ping-number")
                        .long("ping-number")
                        .short('n')
                        .help("Amount of ping packets to send")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u64).range(1..)),
                ),
        )
        .subcommand(
            Command::new("server")
                .about("Receive requests and send them back immediately")
                .arg(
                    Arg::new("local-address")
                        .help("Which address to listen to")
                        .action(ArgAction::Set)
                        .required(true)
                        .value_parser(clap::value_parser!(SocketAddr)),
                ),
        )
        .arg(
            Arg::new("protocol")
                .long("protocol")
                .short('p')
                .help("Set protocol to use for ping")
                .action(ArgAction::Set)
                .value_parser(["tcp", "udp", "icmp"])
                .default_value("udp"),
        )
}

fn main() -> Result<(), io::Error> {
    let matches = cli().get_matches();

    let channel_cap: usize = 32;

    let protocol = matches.get_one::<String>("protocol").unwrap();

    let rt = runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    match matches.subcommand() {
        Some(("client", submatch)) => {
            let remote_address = submatch.get_one::<SocketAddr>("remote-address").unwrap();
            let local_address = submatch.get_one::<SocketAddr>("local-address").unwrap();
            let interval = submatch.get_one::<u64>("interval").unwrap();
            let adaptive = submatch.get_one::<bool>("adaptive-interval").unwrap();
            let wait_time = submatch.get_one::<u64>("wait-time").unwrap();
            let request_size = submatch.get_one::<u16>("req-size").copied();
            let response_size = submatch.get_one::<u16>("resp-size").copied();
            let ping_number = submatch.get_one::<u64>("ping-number").copied();

            let (gen_txtr_send, gen_txtr_recv): (Sender<PingReqResp>, Receiver<PingReqResp>) =
                mpsc::channel(channel_cap);
            let (txtr_stat_send, txtr_stat_recv): (Sender<PingReqResp>, Receiver<PingReqResp>) =
                mpsc::channel(channel_cap);

            let (send_mode, txtr_gen) = if *adaptive {
                let (txtr_gen_send, txtr_gen_recv): (Sender<u8>, Receiver<u8>) =
                    mpsc::channel(channel_cap);

                (SendMode::Adaptive(txtr_gen_recv), Some(txtr_gen_send))
            } else {
                (SendMode::Interval(*interval), None)
            };

            let pinger = match protocol.as_str() {
                "tcp" => rt.spawn(async_tcp::pinger_transport(
                    gen_txtr_recv,
                    txtr_stat_send,
                    *local_address,
                    *remote_address,
                    request_size,
                    response_size,
                )),
                "udp" => rt.spawn(async_udp::pinger_transport(
                    gen_txtr_recv,
                    txtr_stat_send,
                    *local_address,
                    *remote_address,
                    request_size,
                    response_size,
                )),
                "icmp" => unimplemented!(),
                _ => unreachable!(),
            };

            let generator = rt.spawn(pinger::generator(gen_txtr_send, send_mode, ping_number));
            let statista = rt.spawn(statistics::statista(
                txtr_stat_recv,
                txtr_gen,
                Duration::from_millis(*wait_time),
            ));

            rt.block_on(async {
                pinger.await.unwrap();
                generator.await.unwrap();
                statista.await.unwrap();
            });
        }
        Some(("server", submatch)) => {
            let local_address = submatch.get_one::<SocketAddr>("local-address").unwrap();

            let server = match protocol.as_str() {
                "tcp" => rt.spawn(async_tcp::server_transport(*local_address)),
                "udp" => rt.spawn(async_udp::server_transport(*local_address)),
                "icmp" => panic!("there is no server for icmp"),
                _ => unreachable!(),
            };

            rt.block_on(async {
                server.await.unwrap();
            });
        }
        _ => unreachable!(),
    }

    Ok(())
}
