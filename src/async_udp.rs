use std::net::SocketAddr;
use std::collections::HashSet;
use std::time::Instant;

use tokio::sync::mpsc::{Receiver, Sender};
use tokio::net::UdpSocket;

use crate::pinger::{MsgType, PingReqResp};

pub(crate) async fn server_transport(local_address: SocketAddr) {
    let sock = UdpSocket::bind(local_address).await.expect("server: binding failed");

    let mut client_addrs: HashSet<SocketAddr> = HashSet::new();

    loop {
        let mut buf = [0; 8];

        let (amt, addr) = sock.recv_from(&mut buf).await.unwrap();

        if !client_addrs.contains(&addr) {
            println!("New connection from {addr}");
            client_addrs.insert(addr);
        }

        if amt == 8 {
            sock.send_to(&buf, addr).await.unwrap();
        }
        else {
            panic!("server: Received not full number. {amt} instead of 8.")
        }
    }
}

pub(crate) async fn pinger_transport(mut from_generator: Receiver<PingReqResp>,
                                     to_statista: Sender<PingReqResp>,
                                     local_address: SocketAddr,
                                     remote_address: SocketAddr) {
    let sock = UdpSocket::bind(local_address).await.expect("pinger: binding failed");
    sock.connect(remote_address).await.expect("pinger: connect function failed");

    loop {
        let mut buf = [0; 8];

        tokio::select! {
            r_val = from_generator.recv() => {
                match r_val {
                    Some(mut req) => {
                        // Sending request to socket
                        let index = req.index;
                        req.timestamp = Instant::now();

                        sock.send(&index.to_be_bytes()).await.expect("tx: couldn't send message");

                        to_statista.send(req).await.expect("tx: couldn't send transformed request to client");
                    }
                    None => break,
                }
            }
            r_val = sock.recv(&mut buf) => {
                let amt = r_val.unwrap();
                if amt == 8 {
                    let req = PingReqResp {
                        index: u64::from_be_bytes(buf),
                        timestamp: Instant::now(),
                        t: MsgType::Response
                    };

                    to_statista.send(req).await.unwrap();
                }
                else {
                    panic!("rx: Received not full number. {amt} instead of 8.");
                }
            }
        }
    }
}
