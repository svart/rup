use clap::{Arg, ArgAction, Command};
use std::net::SocketAddr;
use std::time::Duration;

use rup::Protocol;
use rup::pinger::PING_HDR_LEN;

fn cli() -> Command {
    Command::new("rup")
        .about("rup universal pinger")
        .version("0.6.2")
        .subcommand_required(true)
        .subcommand(
            Command::new("client")
                .about("Send requests to the remote side and measure RTT")
                .arg(
                    Arg::new("remote-address")
                        .help("Where to send echo requests (host:port)")
                        .action(ArgAction::Set)
                        .required(true),
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
                        .help("Generate new request just after reception of response")
                        .action(ArgAction::SetTrue)
                        .conflicts_with("interval"),
                )
                .arg(
                    Arg::new("wait-time")
                        .long("wait-time")
                        .short('W')
                        .help("Time to wait for response in ms")
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
                    Arg::new("tos")
                        .long("tos")
                        .help("Set outgoing IP TOS / IPv6 traffic class byte")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u8)),
                )
                .arg(
                    Arg::new("ping-number")
                        .long("ping-number")
                        .short('n')
                        .help("Amount of ping packets to send")
                        .action(ArgAction::Set)
                        .value_parser(clap::value_parser!(u64).range(1..)),
                )
                .arg(
                    Arg::new("run-time")
                        .long("run-time")
                        .short('t')
                        .help("Amount of time to send packets")
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
                .value_parser(Protocol::VALUES),
        )
}

fn protocol_or_default(matches: &clap::ArgMatches, default: Protocol) -> Protocol {
    matches
        .get_one::<String>("protocol")
        .map(|protocol| protocol.parse::<Protocol>().unwrap())
        .unwrap_or(default)
}

pub(crate) struct ServerParams {
    pub local_address: SocketAddr,
    pub protocol: Protocol,
}

pub(crate) struct PingerParams {
    pub remote_address: String,
    pub local_address: SocketAddr,
    pub interval: u64,
    pub adaptive: bool,
    pub wait_time: u64,
    pub request_size: Option<u16>,
    pub response_size: Option<u16>,
    pub tos: Option<u8>,
    pub ping_number: Option<u64>,
    pub protocol: Protocol,
    pub run_time: Option<Duration>,
}

pub(crate) enum CliParams {
    ServerParams(ServerParams),
    PingerParams(PingerParams),
}

pub(crate) fn get_cli_params_from<I, T>(args: I) -> Result<CliParams, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let matches = cli().try_get_matches_from(args)?;

    Ok(match matches.subcommand() {
        Some(("client", submatch)) => CliParams::PingerParams(PingerParams {
            remote_address: submatch
                .get_one::<String>("remote-address")
                .unwrap()
                .clone(),
            local_address: *submatch.get_one::<SocketAddr>("local-address").unwrap(),
            interval: *submatch.get_one::<u64>("interval").unwrap(),
            adaptive: *submatch.get_one::<bool>("adaptive-interval").unwrap(),
            wait_time: *submatch.get_one::<u64>("wait-time").unwrap(),
            request_size: submatch.get_one::<u16>("req-size").copied(),
            response_size: submatch.get_one::<u16>("resp-size").copied(),
            tos: submatch.get_one::<u8>("tos").copied(),
            ping_number: submatch.get_one::<u64>("ping-number").copied(),
            protocol: protocol_or_default(&matches, Protocol::Icmp),
            run_time: submatch
                .get_one::<u64>("run-time")
                .map(|d| Duration::from_secs(*d)),
        }),
        Some(("server", submatch)) => CliParams::ServerParams(ServerParams {
            local_address: *submatch.get_one::<SocketAddr>("local-address").unwrap(),
            protocol: protocol_or_default(&matches, Protocol::Udp),
        }),
        _ => unreachable!("clap validates subcommands"),
    })
}

pub(crate) fn get_cli_params() -> CliParams {
    match get_cli_params_from(std::env::args_os()) {
        Ok(params) => params,
        Err(e) => {
            let _ = e.print();
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(args: impl IntoIterator<Item = &'static str>) -> clap::ArgMatches {
        cli().try_get_matches_from(args).unwrap()
    }

    fn try_matches(
        args: impl IntoIterator<Item = &'static str>,
    ) -> Result<clap::ArgMatches, clap::Error> {
        cli().try_get_matches_from(args)
    }

    #[test]
    fn cli_client_defaults() {
        let matches = matches(["rup", "client", "127.0.0.1:5000"]);
        assert_eq!(matches.subcommand_name(), Some("client"));
        let (_, sub) = matches.subcommand().unwrap();
        assert_eq!(
            sub.get_one::<String>("remote-address").unwrap(),
            "127.0.0.1:5000"
        );
        assert_eq!(
            *sub.get_one::<SocketAddr>("local-address").unwrap(),
            SocketAddr::from(([0, 0, 0, 0], 0))
        );
        assert_eq!(*sub.get_one::<u64>("interval").unwrap(), 1000);
        assert!(!*sub.get_one::<bool>("adaptive-interval").unwrap());
        assert_eq!(*sub.get_one::<u64>("wait-time").unwrap(), 1000);
        assert!(sub.get_one::<u16>("req-size").is_none());
        assert!(sub.get_one::<u8>("tos").is_none());
        assert!(sub.get_one::<u64>("ping-number").is_none());
    }

    #[test]
    fn cli_client_all_options() {
        let matches = matches([
            "rup",
            "client",
            "10.0.0.1:8080",
            "--local-address",
            "0.0.0.0:5000",
            "-i",
            "500",
            "-W",
            "2000",
            "--request-size",
            "64",
            "--response-size",
            "128",
            "--tos",
            "184",
            "-n",
            "10",
            "-t",
            "30",
        ]);
        let (_, sub) = matches.subcommand().unwrap();
        assert_eq!(
            sub.get_one::<String>("remote-address").unwrap(),
            "10.0.0.1:8080"
        );
        assert_eq!(*sub.get_one::<u64>("interval").unwrap(), 500);
        assert_eq!(*sub.get_one::<u64>("wait-time").unwrap(), 2000);
        assert_eq!(*sub.get_one::<u16>("req-size").unwrap(), 64);
        assert_eq!(*sub.get_one::<u16>("resp-size").unwrap(), 128);
        assert_eq!(*sub.get_one::<u8>("tos").unwrap(), 184);
        assert_eq!(*sub.get_one::<u64>("ping-number").unwrap(), 10);
        assert_eq!(*sub.get_one::<u64>("run-time").unwrap(), 30);
    }

    #[test]
    fn cli_client_adaptive_mode() {
        let matches = matches(["rup", "client", "127.0.0.1:5000", "-A"]);
        let (_, sub) = matches.subcommand().unwrap();
        assert!(*sub.get_one::<bool>("adaptive-interval").unwrap());
    }

    #[test]
    fn cli_client_interval_conflicts_with_adaptive() {
        assert!(try_matches(["rup", "client", "127.0.0.1:5000", "-i", "500", "-A"]).is_err());
    }

    #[test]
    fn cli_client_req_size_minimum() {
        assert!(try_matches(["rup", "client", "127.0.0.1:5000", "--request-size", "11"]).is_err());
    }

    #[test]
    fn cli_client_req_size_valid() {
        let matches = matches(["rup", "client", "127.0.0.1:5000", "--request-size", "12"]);
        let (_, sub) = matches.subcommand().unwrap();
        assert_eq!(*sub.get_one::<u16>("req-size").unwrap(), 12);
    }

    #[test]
    fn cli_client_tos_valid() {
        let matches = matches(["rup", "client", "127.0.0.1:5000", "--tos", "255"]);
        let (_, sub) = matches.subcommand().unwrap();
        assert_eq!(*sub.get_one::<u8>("tos").unwrap(), 255);
    }

    #[test]
    fn cli_client_tos_invalid() {
        assert!(try_matches(["rup", "client", "127.0.0.1:5000", "--tos", "256"]).is_err());
    }

    #[test]
    fn cli_server_basic() {
        let matches = matches(["rup", "server", "0.0.0.0:5000"]);
        assert_eq!(matches.subcommand_name(), Some("server"));
        let (_, sub) = matches.subcommand().unwrap();
        assert_eq!(
            *sub.get_one::<SocketAddr>("local-address").unwrap(),
            SocketAddr::from(([0, 0, 0, 0], 5000))
        );
    }

    #[test]
    fn cli_protocol_flag_udp() {
        let matches = matches(["rup", "-p", "udp", "client", "127.0.0.1:5000"]);
        assert_eq!(matches.get_one::<String>("protocol").unwrap(), "udp");
    }

    #[test]
    fn cli_protocol_flag_tcp() {
        let matches = matches(["rup", "-p", "tcp", "client", "127.0.0.1:5000"]);
        assert_eq!(matches.get_one::<String>("protocol").unwrap(), "tcp");
    }

    #[test]
    fn cli_protocol_flag_icmp() {
        let matches = matches(["rup", "-p", "icmp", "client", "8.8.8.8"]);
        assert_eq!(matches.get_one::<String>("protocol").unwrap(), "icmp");
    }

    #[test]
    fn cli_client_protocol_defaults_to_icmp() {
        let params = get_cli_params_from(["rup", "client", "8.8.8.8"]).unwrap();

        match params {
            CliParams::PingerParams(params) => {
                assert_eq!(params.protocol, Protocol::Icmp);
            }
            _ => panic!("expected pinger params"),
        }
    }

    #[test]
    fn cli_server_protocol_defaults_to_udp() {
        let params = get_cli_params_from(["rup", "server", "127.0.0.1:5000"]).unwrap();

        match params {
            CliParams::ServerParams(params) => {
                assert_eq!(params.protocol, Protocol::Udp);
            }
            _ => panic!("expected server params"),
        }
    }

    #[test]
    fn cli_protocol_invalid() {
        assert!(try_matches(["rup", "-p", "invalid", "client", "127.0.0.1:5000"]).is_err());
    }

    #[test]
    fn cli_client_no_remote_fails() {
        assert!(try_matches(["rup", "client"]).is_err());
    }

    #[test]
    fn cli_ping_number_and_run_time_both_optional() {
        let matches = matches(["rup", "client", "127.0.0.1:5000"]);
        let (_, sub) = matches.subcommand().unwrap();
        assert!(sub.get_one::<u64>("ping-number").is_none());
        assert!(sub.get_one::<u64>("run-time").is_none());
    }

    #[test]
    fn get_cli_params_from_client_args() {
        let params =
            get_cli_params_from(["rup", "-p", "tcp", "client", "-n", "2", "127.0.0.1:5000"])
                .unwrap();

        match params {
            CliParams::PingerParams(params) => {
                assert_eq!(params.protocol, Protocol::Tcp);
                assert_eq!(params.remote_address, "127.0.0.1:5000");
                assert_eq!(params.ping_number, Some(2));
                assert_eq!(params.tos, None);
            }
            _ => panic!("expected pinger params"),
        }
    }

    #[test]
    fn get_cli_params_from_server_args() {
        let params = get_cli_params_from(["rup", "-p", "udp", "server", "127.0.0.1:5000"]).unwrap();

        match params {
            CliParams::ServerParams(params) => {
                assert_eq!(params.protocol, Protocol::Udp);
                assert_eq!(
                    params.local_address,
                    SocketAddr::from(([127, 0, 0, 1], 5000))
                );
            }
            _ => panic!("expected server params"),
        }
    }

    #[test]
    fn get_cli_params_from_missing_subcommand_errors() {
        assert!(get_cli_params_from(["rup"]).is_err());
    }
}
