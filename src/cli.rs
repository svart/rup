use clap::{Arg, ArgAction, Command};
use std::net::SocketAddr;
use std::time::Duration;

use crate::output_template::ReplyFormat;
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
const ARG_OUTPUT: &str = "output";
const ARG_FORMAT: &str = "format";
const ARG_VERBOSE: &str = "verbose";
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
    ARG_OUTPUT,
    ARG_FORMAT,
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OutputFormat {
    #[default]
    Human,
    Jsonl,
}

fn cli() -> Command {
    Command::new("rup")
        .about("rup universal pinger")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_negates_reqs(true)
        .arg_required_else_help(true)
        .arg(
            Arg::new(ARG_REMOTE_ADDRESS)
                .help("Where to send echo requests (host:port, or URL for http1/http2)")
                .action(ArgAction::Set)
                .required(true),
        )
        .arg(
            Arg::new(ARG_LOCAL_ADDRESS)
                .long("local-address")
                .help("Set local address to bind to")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(SocketAddr)),
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
                .value_parser(clap::value_parser!(u16).range(PacketSize::MIN as i64..)),
        )
        .arg(
            Arg::new(ARG_RESPONSE_SIZE)
                .long("response-size")
                .help("Size of echo response")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u16).range(PacketSize::MIN as i64..)),
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
        .arg(
            Arg::new(ARG_OUTPUT)
                .long("output")
                .help("Select human-readable or JSON Lines client output")
                .action(ArgAction::Set)
                .value_parser(["human", "jsonl"])
                .default_value("human"),
        )
        .arg(
            Arg::new(ARG_FORMAT)
                .long("format")
                .value_name("template")
                .help("Format each human-readable ping reply")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(ReplyFormat)),
        )
        .arg(
            Arg::new(ARG_VERBOSE)
                .long("verbose")
                .short('v')
                .help("Log pipeline diagnostics to stderr")
                .action(ArgAction::SetTrue),
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
                .help("Set protocol to use for ping: icmp, udp, tcp, http1 (alias http), http2")
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
    pub verbose: bool,
}

pub(crate) struct PingerParams {
    pub remote_address: String,
    pub local_address: Option<SocketAddr>,
    pub interval: Duration,
    pub adaptive: bool,
    pub wait_time: Duration,
    pub request_size: Option<PacketSize>,
    pub response_size: Option<PacketSize>,
    pub tos: Option<TrafficClass>,
    pub ping_number: Option<u64>,
    pub protocol: Protocol,
    pub run_time: Option<Duration>,
    pub output: OutputFormat,
    pub format: Option<ReplyFormat>,
    pub verbose: bool,
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

    if matches.get_one::<ReplyFormat>(ARG_FORMAT).is_some()
        && matches.get_one::<String>(ARG_OUTPUT).map(String::as_str) == Some("jsonl")
    {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ArgumentConflict,
            "--format cannot be used with --output jsonl",
        ));
    }

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
                verbose: *matches.get_one::<bool>(ARG_VERBOSE).unwrap(),
            })
        }
        None => CliParams::PingerParams(PingerParams {
            remote_address: matches
                .get_one::<String>(ARG_REMOTE_ADDRESS)
                .unwrap()
                .clone(),
            local_address: matches.get_one::<SocketAddr>(ARG_LOCAL_ADDRESS).copied(),
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
            output: match matches.get_one::<String>(ARG_OUTPUT).map(String::as_str) {
                Some("jsonl") => OutputFormat::Jsonl,
                Some("human") => OutputFormat::Human,
                _ => unreachable!("clap validates output formats"),
            },
            format: matches.get_one::<ReplyFormat>(ARG_FORMAT).cloned(),
            verbose: *matches.get_one::<bool>(ARG_VERBOSE).unwrap(),
        }),
        _ => unreachable!("clap validates subcommands"),
    })
}

pub(crate) fn get_cli_params() -> CliParams {
    // clap exits with 0 for --help/--version and 2 for usage errors.
    get_cli_params_from(std::env::args_os()).unwrap_or_else(|e| e.exit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(args: &[&str]) -> PingerParams {
        match get_cli_params_from(std::iter::once("rup").chain(args.iter().copied())) {
            Ok(CliParams::PingerParams(params)) => params,
            Ok(CliParams::ServerParams(_)) => panic!("expected pinger params for {args:?}"),
            Err(error) => panic!("{args:?} failed to parse: {error}"),
        }
    }

    fn server(args: &[&str]) -> ServerParams {
        match get_cli_params_from(std::iter::once("rup").chain(args.iter().copied())) {
            Ok(CliParams::ServerParams(params)) => params,
            Ok(CliParams::PingerParams(_)) => panic!("expected server params for {args:?}"),
            Err(error) => panic!("{args:?} failed to parse: {error}"),
        }
    }

    fn rejects(args: &[&str]) -> bool {
        get_cli_params_from(std::iter::once("rup").chain(args.iter().copied())).is_err()
    }

    fn size(value: u16) -> Option<PacketSize> {
        Some(PacketSize::new(value).unwrap())
    }

    #[test]
    fn cli_version_matches_package_version() {
        assert_eq!(cli().get_version(), Some(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn client_defaults() {
        let params = client(&["8.8.8.8"]);

        assert_eq!(params.remote_address, "8.8.8.8");
        assert_eq!(params.protocol, Protocol::Icmp);
        assert_eq!(params.local_address, None);
        assert_eq!(params.interval, Duration::from_secs(1));
        assert!(!params.adaptive);
        assert_eq!(params.wait_time, Duration::from_secs(1));
        assert_eq!(params.request_size, None);
        assert_eq!(params.response_size, None);
        assert_eq!(params.tos, None);
        assert_eq!(params.ping_number, None);
        assert_eq!(params.run_time, None);
        assert_eq!(params.output, OutputFormat::Human);
        assert!(params.format.is_none());
        assert!(!params.verbose);
    }

    #[test]
    fn client_all_options() {
        let params = client(&[
            "-p",
            "udp",
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

        assert_eq!(params.remote_address, "10.0.0.1:8080");
        assert_eq!(params.protocol, Protocol::Udp);
        assert_eq!(
            params.local_address,
            Some(SocketAddr::from(([0, 0, 0, 0], 5000)))
        );
        assert_eq!(params.interval, Duration::from_millis(500));
        assert_eq!(params.wait_time, Duration::from_millis(2000));
        assert_eq!(params.request_size, size(64));
        assert_eq!(params.response_size, size(128));
        assert_eq!(params.tos, Some(TrafficClass::new(184)));
        assert_eq!(params.ping_number, Some(10));
        assert_eq!(params.run_time, Some(Duration::from_secs(30)));
    }

    #[test]
    fn client_protocol_flag() {
        for (flag, remote, protocol) in [
            ("udp", "127.0.0.1:5000", Protocol::Udp),
            ("tcp", "127.0.0.1:5000", Protocol::Tcp),
            ("icmp", "8.8.8.8", Protocol::Icmp),
            ("http", "https://example.com", Protocol::Http1),
            ("http2", "https://example.com", Protocol::Http2),
        ] {
            let params = client(&["-p", flag, remote]);
            assert_eq!(params.protocol, protocol, "-p {flag}");
            assert_eq!(params.remote_address, remote);
        }
        assert_eq!(
            client(&["--protocol=http1", "https://example.com"]).protocol,
            Protocol::Http1
        );
        assert!(rejects(&["-p", "invalid", "127.0.0.1:5000"]));
    }

    #[test]
    fn client_adaptive_mode_conflicts_with_interval() {
        assert!(client(&["127.0.0.1:5000", "-A"]).adaptive);
        assert!(rejects(&["127.0.0.1:5000", "-i", "500", "-A"]));
    }

    #[test]
    fn client_packet_size_must_cover_echo_header() {
        assert_eq!(
            client(&["127.0.0.1:5000", "--request-size", "12"]).request_size,
            size(12)
        );
        assert!(rejects(&["127.0.0.1:5000", "--request-size", "11"]));
        assert!(rejects(&["127.0.0.1:5000", "--response-size", "11"]));
    }

    #[test]
    fn client_tos_must_fit_in_a_byte() {
        assert_eq!(
            client(&["127.0.0.1:5000", "--tos", "255"]).tos,
            Some(TrafficClass::new(255))
        );
        assert!(rejects(&["127.0.0.1:5000", "--tos", "256"]));
    }

    #[test]
    fn client_requires_remote_address() {
        assert!(rejects(&[]));
        assert!(rejects(&["-i", "300"]));
    }

    #[test]
    fn client_output_and_reply_format() {
        assert_eq!(
            client(&["--output", "jsonl", "8.8.8.8"]).output,
            OutputFormat::Jsonl
        );
        assert!(
            client(&["8.8.8.8", "--format", "{ip}: {seq} => {rtt}"])
                .format
                .is_some()
        );
        assert!(
            client(&["8.8.8.8", "--output", "human", "--format", "{seq}"])
                .format
                .is_some()
        );
        assert!(rejects(&[
            "8.8.8.8", "--output", "jsonl", "--format", "{seq}"
        ]));
        assert!(rejects(&["8.8.8.8", "--format", "{rtt_ms:.7}"]));
    }

    #[test]
    fn verbose_flag_applies_to_client_and_server() {
        assert!(client(&["-v", "8.8.8.8"]).verbose);
        assert!(client(&["8.8.8.8", "--verbose"]).verbose);
        assert!(server(&["-v", "server", "127.0.0.1:5000"]).verbose);
        assert!(!server(&["server", "127.0.0.1:5000"]).verbose);
    }

    #[test]
    fn server_parses_local_address_and_protocol() {
        let params = server(&["server", "0.0.0.0:5000"]);
        assert_eq!(params.local_address, SocketAddr::from(([0, 0, 0, 0], 5000)));
        assert_eq!(params.protocol, Protocol::Udp);

        assert_eq!(
            server(&["-p", "tcp", "server", "127.0.0.1:5000"]).protocol,
            Protocol::Tcp
        );
    }

    #[test]
    fn server_rejects_client_options() {
        for args in [["-i", "300"], ["--output", "jsonl"], ["--format", "{seq}"]] {
            let args = [&args[..], &["server", "127.0.0.1:5000"]].concat();
            assert!(rejects(&args), "{args:?}");
        }
    }
}
