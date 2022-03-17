// mod tcp;
// mod udp;
// mod client;
// mod server;
// mod pinger;
// mod common;
// mod icmp;

// #[macro_use]
// extern crate clap;

// use clap::App;
// use std::io::Result;

use std::sync::mpsc::{Sender, Receiver};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use std::net::UdpSocket;
use std::collections::VecDeque;
use std::cmp::Ordering;

enum ReqResp {
    REQUEST,
    RESPONSE,
}

struct PingReqResp {
    index: u64,
    timestamp: Instant,
    t: ReqResp,
}


fn tx_transport_thread(from_client: Receiver<PingReqResp>,
                       to_client: Sender<PingReqResp>) {
    let socket = UdpSocket::bind("127.0.0.1:55555").expect("tx: binding failed");
    socket.connect("127.0.0.1:44444").expect("tx: connect function failed");

    loop {
        match from_client.recv() {
            Ok(mut req) => {
                let index = req.index;
                req.timestamp = Instant::now();
                println!("tx: Received {index} from client");
                socket.send(&index.to_be_bytes()).expect("tx: couldn't send message");
                to_client.send(req).expect("tx: couldn't send transformed request to client");
            }
            Err(err) => {
                println!("tx: Got error during recv from client: {err}");
                break;
            }
        }
    }
}

fn rx_transport_thread(to_client: Sender<PingReqResp>) {
    let socket = UdpSocket::bind("127.0.0.1:44444").unwrap();

    loop {
        let mut buf = [0; 8];
        let amt = socket.recv(&mut buf).unwrap();
        if amt == 8 {
            let num = u64::from_be_bytes(buf);
            println!("rx: Received {num} from socket, sending to client");
            to_client.send(PingReqResp{ index: num, timestamp: Instant::now(), t: ReqResp::RESPONSE }).unwrap();
        }
        else {
            println!("rx: Received not full number. {amt} instead of 8.");
            break;
        }
    }
}

fn client_generator(to_tx_transport: Sender<PingReqResp>) {
    for i in 0..20 {
        match to_tx_transport.send(PingReqResp{index: i, timestamp: Instant::now(), t: ReqResp::REQUEST}) {
            Ok(_) => println!("client: Sent {i} to transport"),
            Err(err) => panic!("client: Error during sending {i} to transport: {err}"),
        }

        thread::sleep(Duration::from_millis(500));
    }
}

fn client_statistic(from_transport: Receiver<PingReqResp>) {
    let mut requests: VecDeque<PingReqResp> = VecDeque::new();

    loop {
        match from_transport.recv() {
            Ok(resp) => {
                match resp.t {
                    ReqResp::REQUEST => {
                        requests.push_back(resp);
                    },
                    ReqResp::RESPONSE => {
                        let index = resp.index;
                        println!("client: Received {index} from transport");

                        while let Some(req) = requests.pop_front() {
                            match index.cmp(&req.index) {
                                Ordering::Greater => continue,
                                Ordering::Equal => println!("Seq: {index} rtt: {:#?}", resp.timestamp.duration_since(req.timestamp)),
                                Ordering::Less => requests.push_front(req),
                            }
                            break;
                        }
                    },
                }
            }
            Err(err) => {
                println!("client: Error during recv from transport: {err}");
                break;
            }
        }
        println!();
    }
}

fn main() {
    let (cl_txtr_tx, cl_txtr_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel();
    let (txtr_cl_tx, tr_cl_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel();
    let rxtr_cl_tx = txtr_cl_tx.clone();


    let generator = thread::spawn(move || client_generator(cl_txtr_tx));
    let tx_transport = thread::spawn(move || tx_transport_thread(cl_txtr_rx, txtr_cl_tx));
    let rx_transport = thread::spawn(move || rx_transport_thread(rxtr_cl_tx));
    let statista = thread::spawn(move || client_statistic(tr_cl_rx));

    generator.join().expect("oops, generator crashed");
    tx_transport.join().expect("oops, tx transport crashed");
    rx_transport.join().expect("oops, rx transport crashed");
    statista.join().expect("oops, statista crashed")
}
