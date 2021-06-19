use std::io::Result;
use std::str;
use std::time::Instant;
use std::collections::VecDeque;

use mio::net::UdpSocket;
use mio::Events;

use std::{
    net::Ipv4Addr,
    time::{Duration},
};

use icmp_socket::packet::WithEchoRequest;
use icmp_socket::socket::IcmpSocket;
use icmp_socket::*;

use crate::client::{Client, TimeStamp};
use crate::pinger::{Pinger, PING_MSG_LEN};
use crate::server::setup_server_polling;


fn packet_handler(pkt: Icmpv4Packet, send_time: Instant) -> Option<()> {
    let now = Instant::now();
    let elapsed = now - send_time;
    if let Icmpv4Message::EchoReply {
        identifier: _,
        sequence: _,
        payload: _,
    } = pkt.message
    {
        println!( "RTT = {}", elapsed.as_micros());
    } else {
        //eprintln!("Discarding non-reply {:?}", pkt);
        return None;
    }
    Some(())
}

pub fn run_client(address: &str, interval: Option<u64>) -> Result<()> {
    println!("Running ICMP client sending pings to {}", address);

    let remote_addr = address.to_string().parse::<Ipv4Addr>().unwrap();
    let mut socket4 = IcmpSocket4::new()?;
    socket4.bind("0.0.0.0".parse::<Ipv4Addr>().unwrap())?;
    // TODO(jwall): The first packet we receive will be the one we sent.
    // We need to implement packet filtering for the socket.
    let mut sequence: u64 = 0;
    loop {
        let packet = Icmpv4Packet::with_echo_request(
            42,
            sequence as u16,
            (0..64).map(|x| x).collect(),
        )
            .unwrap();
        let send_time = Instant::now();
        socket4.send_to(remote_addr, packet)?;
        socket4.set_timeout(Duration::from_secs(1))?;
        loop {
            let (resp, sock_addr) = match socket4.rcv_from() {
                Ok(tpl) => tpl,
                Err(e) => {
                    eprintln!("{:?}", e);
                    break;
                }
            };

            let recv_addr = *sock_addr.as_inet().unwrap().ip();
            if recv_addr == remote_addr {
                if packet_handler(resp, send_time).is_some() {
                    if interval.is_some() {
                        std::thread::sleep(Duration::from_millis(interval.unwrap()));
                    }
                    break;
                }
            } else {
                eprintln!("Discarding packet from {}", recv_addr);
            }
        }
        sequence = sequence.wrapping_add(1);
    }
}