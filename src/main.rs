use std::time::{Duration, Instant};
use std::collections::VecDeque;
use std::cmp::Ordering;
use std::io;

use tokio::{self, time};
use tokio::sync::mpsc::{self, Sender, Receiver};
use tokio::net::UdpSocket;

use clap::{Command, Arg, ArgAction};

#[derive(Debug)]
enum MsgType {
    REQUEST,
    RESPONSE,
}

#[derive(Debug)]
struct PingReqResp {
    index: u64,
    timestamp: Instant,
    t: MsgType,
}

struct PingRTT {
    index: u64,
    rtt: Duration,
}


async fn transport_thread(mut from_client: Receiver<PingReqResp>,
                          to_client: Sender<PingReqResp>) {
    let tx_sock = UdpSocket::bind("127.0.0.1:55555").await.expect("tx: binding failed");
    let rx_sock = UdpSocket::bind("127.0.0.1:44444").await.unwrap();
    tx_sock.connect("127.0.0.1:44444").await.expect("tx: connect function failed");

    loop {
        let mut buf = [0; 8];

        tokio::select! {
            r_val = from_client.recv() => {
                match r_val {
                    Some(mut req) => {
                        // Sending request to socket
                        let index = req.index;
                        req.timestamp = Instant::now();

                        tx_sock.send(&index.to_be_bytes()).await.expect("tx: couldn't send message");

                        to_client.send(req).await.expect("tx: couldn't send transformed request to client");
                    }
                    None => break,
                }
            }
            r_val = rx_sock.recv(&mut buf) => {
                let amt = r_val.unwrap();
                if amt == 8 {
                    let req = PingReqResp {
                        index: u64::from_be_bytes(buf),
                        timestamp: Instant::now(),
                        t: MsgType::RESPONSE
                    };

                    to_client.send(req).await.unwrap();
                }
                else {
                    panic!("rx: Received not full number. {amt} instead of 8.");
                }
            }
        }
    }
}

async fn generator_thread(to_tx_transport: Sender<PingReqResp>) {
    for i in 0..20 {
        let req = PingReqResp {
            index: i,
            timestamp: Instant::now(),
            t: MsgType::REQUEST
        };

        if let Err(err) = to_tx_transport.send(req).await {
            panic!("client: Error during sending {i} to transport: {err}");
        }

        time::sleep(Duration::from_millis(500)).await;
    }
}

async fn statista_thread(mut from_transport: Receiver<PingReqResp>,
                         to_presenter: Sender<PingRTT>) {
    let mut requests: VecDeque<PingReqResp> = VecDeque::new();

    loop {
        if let Some(resp) = from_transport.recv().await {
            match resp.t {
                MsgType::REQUEST => {
                    requests.push_back(resp);
                },
                MsgType::RESPONSE => {
                    let index = resp.index;

                    while let Some(req) = requests.pop_front() {
                        match index.cmp(&req.index) {
                            Ordering::Greater => continue,
                            Ordering::Equal => {
                                let timestamp = PingRTT {
                                    index,
                                    rtt: resp.timestamp.duration_since(req.timestamp)
                                };
                                to_presenter.send(timestamp).await;
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

async fn presenter_thread(mut from_statista: Receiver<PingRTT>) {
    loop {
        if let Some(rtt) = from_statista.recv().await {
            println!("Seq: {} rtt: {:#?}", rtt.index, rtt.rtt);
        }
        else {
            break;
        }
    }
}

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
                )
        )
        .subcommand(
            Command::new("server")
                .about("Receive requests and send them back immediately")
        )
        .arg(
            Arg::new("local-address")
                .long("local-address")
                .help("Set local address to bind to")
                .action(ArgAction::Set)
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
        .arg(
            Arg::new("interval")
                .long("interval")
                .short('i')
                .help("Set interval in ms to send echo requests")
                .action(ArgAction::Set)
                .value_parser(clap::value_parser!(u32).range(1..))
                .default_value("1000")
        )
}

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    let matches = cli().get_matches();
    let channel_cap: usize = 32;

    let (cl_txtr_tx, cl_txtr_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel(channel_cap);
    let (txtr_cl_tx, tr_cl_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel(channel_cap);
    let (st_pr_tx, st_pr_rx): (Sender<PingRTT>, Receiver<PingRTT>) = mpsc::channel(channel_cap);

    let generator = tokio::spawn(generator_thread(cl_txtr_tx));
    let transport = tokio::spawn(transport_thread(cl_txtr_rx, txtr_cl_tx));
    let statista = tokio::spawn(statista_thread(tr_cl_rx, st_pr_tx));
    let presenter = tokio::spawn(presenter_thread(st_pr_rx));

    tokio::join!(generator, transport, statista, presenter);
    Ok(())
}
