mod tcp;
mod udp;
mod client;
mod server;
mod pinger;
mod common;
mod icmp;

#[macro_use]
extern crate clap;

use clap::App;
use std::io::Result;

// TODO: Set tos on the messages
// TODO: Set length of the messages

fn main() -> Result<()> {
    let yaml = load_yaml!("cli.yml");
    let matches = App::from_yaml(yaml).get_matches();

    let protocol = matches.value_of("protocol").unwrap_or("icmp");
    let server_mode = matches.is_present("server");
    let client_mode = matches.is_present("client");
    let interval: Option<u64> = matches.value_of("interval").map(|value| value.parse().unwrap());

    if protocol == "icmp" {
        let remote_address = matches.value_of("remote-address").unwrap();
        icmp::run_client(remote_address, interval)
    } else if server_mode {
        let local_address = matches.value_of("local-address").unwrap_or("0.0.0.0");
        let local_port = matches.value_of("local-port").unwrap();

        match protocol {
            "tcp" => tcp::run_server(local_address, local_port),
            "udp" => udp::run_server(local_address, local_port),
            _ => unimplemented!(),
        }
    } else if client_mode {
        let remote_address = matches.value_of("remote-address").unwrap();
        let remote_port = matches.value_of("remote-port").unwrap();

        match protocol {
            "tcp" => tcp::run_client(remote_address, remote_port, interval),
            "udp" => udp::run_client(remote_address, remote_port, interval),
            _ => unimplemented!(),
        }
    } else {
        panic!("Mode should be set or protocol should be icmp!");
    }
}
