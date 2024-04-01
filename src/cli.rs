use std::net::SocketAddr;

use clap::{Arg, ArgAction, Command};

use crate::pinger::PING_HDR_LEN;

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

pub(crate) struct ServerParams {
    pub local_address: SocketAddr,
    pub protocol: String,
}

pub(crate) struct PingerParams {
    pub remote_address: SocketAddr,
    pub local_address: SocketAddr,
    pub interval: u64,
    pub adaptive: bool,
    pub wait_time: u64,
    pub request_size: Option<u16>,
    pub response_size: Option<u16>,
    pub ping_number: Option<u64>,
    pub protocol: String,
}

pub(crate) enum CliParams {
    ServerParams(ServerParams),
    PingerParams(PingerParams),
}

pub(crate) fn get_cli_params() -> CliParams {
    let matches = cli().get_matches();

    let protocol = matches.get_one::<String>("protocol").unwrap();

    match matches.subcommand() {
        Some(("client", submatch)) => {
            CliParams::PingerParams(PingerParams {
                remote_address: *submatch.get_one::<SocketAddr>("remote-address").unwrap(),
                local_address: *submatch.get_one::<SocketAddr>("local-address").unwrap(),
                interval: *submatch.get_one::<u64>("interval").unwrap(),
                adaptive: *submatch.get_one::<bool>("adaptive-interval").unwrap(),
                wait_time: *submatch.get_one::<u64>("wait-time").unwrap(),
                request_size: submatch.get_one::<u16>("req-size").copied(),
                response_size: submatch.get_one::<u16>("resp-size").copied(),
                ping_number: submatch.get_one::<u64>("ping-number").copied(),
                protocol: protocol.clone(),
            })
        }
        Some(("server", submatch)) => {
            CliParams::ServerParams(ServerParams{
                local_address: *submatch.get_one::<SocketAddr>("local-address").unwrap(),
                protocol: protocol.clone(),
            })

        }
        _ => unreachable!(),
    }
}