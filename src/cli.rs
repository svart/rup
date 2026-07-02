use clap::{Arg, ArgAction, Command};
use std::net::SocketAddr;
use std::time::Duration;

use rup::pinger::PING_HDR_LEN;
use rup::{PacketSize, Protocol, TrafficClass};

const ARG_REMOTE_ADDRESS: &str = "remote-address";
const ARG_LOCAL_ADDRESS: &str = "local-address";
const ARG_INTERVAL: &str = "interval";
const ARG_ADAPTIVE_INTERVAL: &str = "adaptive-interval";
const ARG_WAIT_TIME: &str = "wait-time";
const ARG_REQUEST_SIZE: &str = "req-size";
const ARG_RESPONSE_SIZE: &str = "resp-size";
const ARG_TOS: &str = "tos";
const ARG_PING_NUMBER: &str = "ping-number";
const ARG_RUN_TIME: &str = "run-time";
const ARG_PROTOCOL: &str = "protocol";
const CMD_SERVER: &str = "server";

const CLIENT_ARGS: &[&str] = &[
    ARG_REMOTE_ADDRESS,
    ARG_LOCAL_ADDRESS,
    ARG_INTERVAL,
    ARG_ADAPTIVE_INTERVAL,
    ARG_WAIT_TIME,
    ARG_REQUEST_SIZE,
    ARG_RESPONSE_SIZE,
    ARG_TOS,
    ARG_PING_NUMBER,
    ARG_RUN_TIME,
];

fn cli() -> Command {
    Command::new("rup")
        .about("rup universal pinger")
        .version("0.11.0")
        .subcommand_negates_reqs(true)
        .arg_required_else_help(true)
        .arg(
            Arg::new(ARG_REMOTE_ADDRESS)
                .help("Where to send echo requests (host:port)")
                .action(ArgAction::Set)
                .required(true),
        )
        .arg(
            Arg::new(ARG_LOCAL_ADDRESS)
                .long("local-address")
                .help("Set local address to bind to")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(SocketAddr))
                .default_value("0.0.0.0:0"),
        )
        .arg(
            Arg::new(ARG_INTERVAL)
                .long("interval")
                .short('i')
                .help("Set interval in ms to send echo requests")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u64).range(1..))
                .default_value("1000")
                .conflicts_with(ARG_ADAPTIVE_INTERVAL),
        )
        .arg(
            Arg::new(ARG_ADAPTIVE_INTERVAL)
                .long("adaptive-interval")
                .short('A')
                .help("Generate new request just after reception of response")
                .action(ArgAction::SetTrue)
                .conflicts_with(ARG_INTERVAL),
        )
        .arg(
            Arg::new(ARG_WAIT_TIME)
                .long("wait-time")
                .short('W')
                .help("Time to wait for response in ms")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u64).range(1..))
                .default_value("1000"),
        )
        .arg(
            Arg::new(ARG_REQUEST_SIZE)
                .long("request-size")
                .help("Size of echo request")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u16).range(PING_HDR_LEN as i64..)),
        )
        .arg(
            Arg::new(ARG_RESPONSE_SIZE)
                .long("response-size")
                .help("Size of echo response")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u16).range(PING_HDR_LEN as i64..)),
        )
        .arg(
            Arg::new(ARG_TOS)
                .long("tos")
                .help("Set outgoing IP TOS / IPv6 traffic class byte")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u8)),
        )
        .arg(
            Arg::new(ARG_PING_NUMBER)
                .long("ping-number")
                .short('n')
                .help("Amount of ping packets to send")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u64).range(1..)),
        )
        .arg(
            Arg::new(ARG_RUN_TIME)
                .long("run-time")
                .short('t')
                .help("Amount of time to send packets")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u64).range(1..)),
        )
        .subcommand(
            Command::new(CMD_SERVER)
                .about("Receive requests and send them back immediately")
                .arg(
                    Arg::new(ARG_LOCAL_ADDRESS)
                        .help("Which address to listen to")
                        .action(ArgAction::Set)
                        .required(true)
                        .value_parser(clap::value_parser!(SocketAddr)),
                ),
        )
        .arg(
            Arg::new(ARG_PROTOCOL)
                .long("protocol")
                .short('p')
                .help("Set protocol to use for ping")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(Protocol)),
        )
}

fn client_arg_used(matches: &clap::ArgMatches) -> bool {
    CLIENT_ARGS
        .iter()
        .any(|arg| matches.value_source(arg) == Some(clap::parser::ValueSource::CommandLine))
}

pub(crate) struct ServerParams {
    pub local_address: SocketAddr,
    pub protocol: Protocol,
}

pub(crate) struct PingerParams {
    pub remote_address: String,
    pub local_address: SocketAddr,
    pub interval: Duration,
    pub adaptive: bool,
    pub wait_time: Duration,
    pub request_size: Option<PacketSize>,
    pub response_size: Option<PacketSize>,
    pub tos: Option<TrafficClass>,
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
        Some((CMD_SERVER, submatch)) => {
            if client_arg_used(&matches) {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ArgumentConflict,
                    "client options cannot be used with the server subcommand",
                ));
            }

            CliParams::ServerParams(ServerParams {
                local_address: *submatch.get_one::<SocketAddr>(ARG_LOCAL_ADDRESS).unwrap(),
                protocol: matches
                    .get_one::<Protocol>(ARG_PROTOCOL)
                    .copied()
                    .unwrap_or(Protocol::Udp),
            })
        }
        None => CliParams::PingerParams(PingerParams {
            remote_address: matches
                .get_one::<String>(ARG_REMOTE_ADDRESS)
                .unwrap()
                .clone(),
            local_address: *matches.get_one::<SocketAddr>(ARG_LOCAL_ADDRESS).unwrap(),
            interval: Duration::from_millis(*matches.get_one::<u64>(ARG_INTERVAL).unwrap()),
            adaptive: *matches.get_one::<bool>(ARG_ADAPTIVE_INTERVAL).unwrap(),
            wait_time: Duration::from_millis(*matches.get_one::<u64>(ARG_WAIT_TIME).unwrap()),
            request_size: matches
                .get_one::<u16>(ARG_REQUEST_SIZE)
                .copied()
                .map(|size| PacketSize::new(size).expect("clap validates packet size")),
            response_size: matches
                .get_one::<u16>(ARG_RESPONSE_SIZE)
                .copied()
                .map(|size| PacketSize::new(size).expect("clap validates packet size")),
            tos: matches
                .get_one::<u8>(ARG_TOS)
                .copied()
                .map(TrafficClass::new),
            ping_number: matches.get_one::<u64>(ARG_PING_NUMBER).copied(),
            protocol: matches
                .get_one::<Protocol>(ARG_PROTOCOL)
                .copied()
                .unwrap_or(Protocol::Icmp),
            run_time: matches
                .get_one::<u64>(ARG_RUN_TIME)
                .map(|d| Duration::from_secs(*d)),
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
        let matches = matches(["rup", "127.0.0.1:5000"]);
        assert_eq!(matches.subcommand_name(), None);
        assert_eq!(
            matches.get_one::<String>("remote-address").unwrap(),
            "127.0.0.1:5000"
        );
        assert_eq!(
            *matches.get_one::<SocketAddr>("local-address").unwrap(),
            SocketAddr::from(([0, 0, 0, 0], 0))
        );
        assert_eq!(*matches.get_one::<u64>("interval").unwrap(), 1000);
        assert!(!*matches.get_one::<bool>("adaptive-interval").unwrap());
        assert_eq!(*matches.get_one::<u64>("wait-time").unwrap(), 1000);
        assert!(matches.get_one::<u16>("req-size").is_none());
        assert!(matches.get_one::<u8>("tos").is_none());
        assert!(matches.get_one::<u64>("ping-number").is_none());
    }

    #[test]
    fn cli_client_all_options() {
        let matches = matches([
            "rup",
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
        assert_eq!(
            matches.get_one::<String>("remote-address").unwrap(),
            "10.0.0.1:8080"
        );
        assert_eq!(*matches.get_one::<u64>("interval").unwrap(), 500);
        assert_eq!(*matches.get_one::<u64>("wait-time").unwrap(), 2000);
        assert_eq!(*matches.get_one::<u16>("req-size").unwrap(), 64);
        assert_eq!(*matches.get_one::<u16>("resp-size").unwrap(), 128);
        assert_eq!(*matches.get_one::<u8>("tos").unwrap(), 184);
        assert_eq!(*matches.get_one::<u64>("ping-number").unwrap(), 10);
        assert_eq!(*matches.get_one::<u64>("run-time").unwrap(), 30);
    }

    #[test]
    fn cli_client_adaptive_mode() {
        let matches = matches(["rup", "127.0.0.1:5000", "-A"]);
        assert!(*matches.get_one::<bool>("adaptive-interval").unwrap());
    }

    #[test]
    fn cli_client_interval_conflicts_with_adaptive() {
        assert!(try_matches(["rup", "127.0.0.1:5000", "-i", "500", "-A"]).is_err());
    }

    #[test]
    fn cli_client_req_size_minimum() {
        assert!(try_matches(["rup", "127.0.0.1:5000", "--request-size", "11"]).is_err());
    }

    #[test]
    fn cli_client_req_size_valid() {
        let matches = matches(["rup", "127.0.0.1:5000", "--request-size", "12"]);
        assert_eq!(*matches.get_one::<u16>("req-size").unwrap(), 12);
    }

    #[test]
    fn cli_client_tos_valid() {
        let matches = matches(["rup", "127.0.0.1:5000", "--tos", "255"]);
        assert_eq!(*matches.get_one::<u8>("tos").unwrap(), 255);
    }

    #[test]
    fn cli_client_tos_invalid() {
        assert!(try_matches(["rup", "127.0.0.1:5000", "--tos", "256"]).is_err());
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
    fn cli_server_rejects_client_options() {
        assert!(get_cli_params_from(["rup", "-i", "300", "server", "0.0.0.0:5000"]).is_err());
    }

    #[test]
    fn cli_protocol_flag_udp() {
        let matches = matches(["rup", "-p", "udp", "127.0.0.1:5000"]);
        assert_eq!(
            *matches.get_one::<Protocol>("protocol").unwrap(),
            Protocol::Udp
        );
    }

    #[test]
    fn cli_protocol_flag_tcp() {
        let matches = matches(["rup", "-p", "tcp", "127.0.0.1:5000"]);
        assert_eq!(
            *matches.get_one::<Protocol>("protocol").unwrap(),
            Protocol::Tcp
        );
    }

    #[test]
    fn cli_protocol_flag_icmp() {
        let matches = matches(["rup", "-p", "icmp", "8.8.8.8"]);
        assert_eq!(
            *matches.get_one::<Protocol>("protocol").unwrap(),
            Protocol::Icmp
        );
    }

    #[test]
    fn cli_client_protocol_defaults_to_icmp() {
        let params = get_cli_params_from(["rup", "8.8.8.8"]).unwrap();

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
        assert!(try_matches(["rup", "-p", "invalid", "127.0.0.1:5000"]).is_err());
    }

    #[test]
    fn cli_client_no_remote_fails() {
        assert!(try_matches(["rup", "-i", "300"]).is_err());
    }

    #[test]
    fn cli_ping_number_and_run_time_both_optional() {
        let matches = matches(["rup", "127.0.0.1:5000"]);
        assert!(matches.get_one::<u64>("ping-number").is_none());
        assert!(matches.get_one::<u64>("run-time").is_none());
    }

    #[test]
    fn get_cli_params_from_client_args() {
        let params = get_cli_params_from(["rup", "127.0.0.1", "-i", "300"]).unwrap();

        match params {
            CliParams::PingerParams(params) => {
                assert_eq!(params.protocol, Protocol::Icmp);
                assert_eq!(params.remote_address, "127.0.0.1");
                assert_eq!(params.interval, Duration::from_millis(300));
            }
            _ => panic!("expected pinger params"),
        }
    }

    #[test]
    fn get_cli_params_from_default_client_args() {
        let params =
            get_cli_params_from(["rup", "-p", "tcp", "-n", "2", "127.0.0.1:5000"]).unwrap();

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
    fn get_cli_params_from_missing_remote_errors() {
        assert!(get_cli_params_from(["rup"]).is_err());
    }
}
