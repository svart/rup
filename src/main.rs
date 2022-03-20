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

struct PingRTT {
    index: u64,
    rtt: Duration,
}


fn transport_thread(from_client: Receiver<PingReqResp>,
             to_client: Sender<PingReqResp>) {
    let tx_sock = UdpSocket::bind("127.0.0.1:55555").expect("tx: binding failed");
    let rx_sock = UdpSocket::bind("127.0.0.1:44444").unwrap();
    tx_sock.connect("127.0.0.1:44444").expect("tx: connect function failed");

    loop {
        if let Ok(mut req) = from_client.recv() {
            // Sending request to socket
            let index = req.index;
            req.timestamp = Instant::now();

            tx_sock.send(&index.to_be_bytes()).expect("tx: couldn't send message");

            to_client.send(req).expect("tx: couldn't send transformed request to client");

            // Receiving request from socket
            let mut buf = [0; 8];
            let amt = rx_sock.recv(&mut buf).unwrap();

            if amt == 8 {
                let req = PingReqResp {
                    index: u64::from_be_bytes(buf),
                    timestamp: Instant::now(),
                    t: ReqResp::RESPONSE
                };

                to_client.send(req).unwrap();
            }
            else {
                panic!("rx: Received not full number. {amt} instead of 8.");
            }
        }
        else {
            break;
        }
    }
}

fn generator_thread(to_tx_transport: Sender<PingReqResp>) {
    for i in 0..20 {
        let req = PingReqResp{
            index: i,
            timestamp: Instant::now(),
            t: ReqResp::REQUEST
        };

        if let Err(err) = to_tx_transport.send(req) {
            panic!("client: Error during sending {i} to transport: {err}");
        }

        thread::sleep(Duration::from_millis(500));
    }
}

fn statista_thread(from_transport: Receiver<PingReqResp>, to_presenter: Sender<PingRTT>) {
    let mut requests: VecDeque<PingReqResp> = VecDeque::new();

    loop {
        if let Ok(resp) = from_transport.recv() {
            match resp.t {
                ReqResp::REQUEST => {
                    requests.push_back(resp);
                },
                ReqResp::RESPONSE => {
                    let index = resp.index;

                    while let Some(req) = requests.pop_front() {
                        match index.cmp(&req.index) {
                            Ordering::Greater => continue,
                            Ordering::Equal => {
                                let timestamp = PingRTT {
                                    index,
                                    rtt: resp.timestamp.duration_since(req.timestamp)
                                };
                                to_presenter.send(timestamp).unwrap();
                            }
                            Ordering::Less => requests.push_front(req),
                        }
                        break;
                    }
                },
            }
        }
        else {
            break;
        }
    }
}

fn presenter_thread(from_statista: Receiver<PingRTT>) {
    loop {
        if let Ok(rtt) = from_statista.recv() {
            println!("Seq: {} rtt: {:#?}", rtt.index, rtt.rtt);
        }
        else {
            break;
        }
    }
}

fn main() {
    let (cl_txtr_tx, cl_txtr_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel();
    let (txtr_cl_tx, tr_cl_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel();
    let (st_pr_tx, st_pr_rx): (Sender<PingRTT>, Receiver<PingRTT>) = mpsc::channel();

    let generator = thread::spawn(move || generator_thread(cl_txtr_tx));
    let transport = thread::spawn(move || transport_thread(cl_txtr_rx, txtr_cl_tx));
    let statista = thread::spawn(move || statista_thread(tr_cl_rx, st_pr_tx));
    let presenter = thread::spawn(move || presenter_thread(st_pr_rx));

    generator.join().expect("oops, generator crashed");
    transport.join().expect("oops, tx transport crashed");
    statista.join().expect("oops, statista crashed");
    presenter.join().expect("oops, presenter crashed");
}
