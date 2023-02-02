use std::net::SocketAddr;
use std::io;
use std::time::Duration;

use tokio;
use tokio::sync::mpsc::{self, Sender, Receiver};

use clap::{Command, Arg, ArgAction};

mod async_udp;
mod async_tcp;
mod pinger;
mod statistics;

use pinger::PingReqResp;
use statistics::PingRTT;


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
                        .value_parser(clap::value_parser!(SocketAddr))
                )
                .arg(
                    Arg::new("local-address")
                        .long("local-address")
                        .help("Set local address to bind to")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(SocketAddr))
                        .default_value("0.0.0.0:0")
                )
                .arg(
                    Arg::new("interval")
                        .long("interval")
                        .short('i')
                        .help("Set interval in ms to send echo requests")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u64).range(1..))
                        .default_value("1000")
                )
                .arg(
                    Arg::new("wait-time")
                        .long("wait-time")
                        .short('W')
                        .help("Time to wait for responce im ms")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u64).range(1..))
                        .default_value("1000")
                )
        )
        .subcommand(
            Command::new("server")
                .about("Receive requests and send them back immediately")
                .arg(
                    Arg::new("local-address")
                        .help("Which address to listen to")
                        .action(ArgAction::Set)
                        .required(true)
                        .value_parser(clap::value_parser!(SocketAddr))
                )
        )
        .arg(
            Arg::new("protocol")
                .long("protocol")
                .short('p')
                .help("Set protocol to use for ping")
                .action(ArgAction::Set)
                .value_parser(["tcp", "udp", "icmp"])
                .default_value("udp")
        )
}

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    let matches = cli().get_matches();

    let channel_cap: usize = 32;

    let protocol = matches.get_one::<String>("protocol").unwrap();

    match matches.subcommand() {
        Some(("client", submatch)) => {
            let remote_address = submatch.get_one::<SocketAddr>("remote-address").unwrap();
            let local_address = submatch.get_one::<SocketAddr>("local-address").unwrap();
            let interval = submatch.get_one::<u64>("interval").unwrap();
            let wait_time = submatch.get_one::<u64>("wait-time").unwrap();

            let (cl_txtr_tx, cl_txtr_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel(channel_cap);
            let (txtr_cl_tx, tr_cl_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel(channel_cap);
            let (st_pr_tx, st_pr_rx): (Sender<PingRTT>, Receiver<PingRTT>) = mpsc::channel(channel_cap);

            let pinger = match protocol.as_str() {
                "tcp" => tokio::spawn(async_tcp::pinger_transport(cl_txtr_rx, txtr_cl_tx, *local_address, *remote_address)),
                "udp" => tokio::spawn(async_udp::pinger_transport(cl_txtr_rx, txtr_cl_tx, *local_address, *remote_address)),
                "icmp" => unimplemented!(),
                _ => unreachable!(),
            };

            let generator = tokio::spawn(pinger::generator(cl_txtr_tx, *interval));
            let statista = tokio::spawn(statistics::statista(tr_cl_rx, st_pr_tx, Duration::from_millis(*wait_time)));
            let presenter = tokio::spawn(statistics::presenter(st_pr_rx));

            let _ = tokio::join!(pinger, generator, statista, presenter);
        },
        Some(("server", submatch)) => {
            let local_address = submatch.get_one::<SocketAddr>("local-address").unwrap();

            let server = match protocol.as_str() {
                "tcp" => tokio::spawn(async_tcp::server_transport(*local_address)),
                "udp" => tokio::spawn(async_udp::server_transport(*local_address)),
                "icmp" => panic!("there is no server for icmp"),
                _ => unreachable!(),
            };

            let _ = tokio::join!(server);
        },
        _ => unreachable!()
    }

    Ok(())
}
