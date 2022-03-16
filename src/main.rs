use std::sync::mpsc::{Sender, Receiver};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::net::UdpSocket;

struct PingReqResp {
    index: u32,
}


fn tx_transport_thread(rx: Receiver<PingReqResp>) {
    let socket = UdpSocket::bind("127.0.0.1:55555").expect("binding failed");
    socket.connect("127.0.0.1:44444").expect("connect function failed");

    loop {
        match rx.recv() {
            Ok(PingReqResp { index }) => {
                println!("tx: Received {index} from client");
                socket.send(&index.to_be_bytes()).expect("couldn't send message");
            }
            Err(err) => {
                println!("tx: Got error during recv from client: {err}");
                break;
            }
        }
    }
}

fn as_u32_be(array: &[u8; 4]) -> u32 {
    ((array[0] as u32) << 24) +
    ((array[1] as u32) << 16) +
    ((array[2] as u32) <<  8) +
    ((array[3] as u32) <<  0)
}

fn rx_transport_thread(tx: Sender<PingReqResp>) {
    let socket = UdpSocket::bind("127.0.0.1:44444").unwrap();

    loop {
        let mut buf = [0; 4];
        let amt = socket.recv(&mut buf).unwrap();
        if amt == 4 {
            let num = as_u32_be(&buf);
            println!("rx: Received {num} from socket, sending to client");
            tx.send(PingReqResp{ index: num }).unwrap();
        }
        else {
            println!("rx: Received not full number. {amt} instead of 4.");
            break;
        }
    }
}

fn client_thread(to_transport: Sender<PingReqResp>, from_transport: Receiver<PingReqResp>) {
    for i in 0..20 {
        thread::sleep(Duration::from_millis(500));

        match to_transport.send(PingReqResp{index: i}) {
            Ok(_) => println!("client: Sent {i} to transport"),
            Err(err) => {
                println!("client: Error during sending {i} to transport: {err}");
                break;
            }
        }

        match from_transport.recv() {
            Ok(PingReqResp { index }) => println!("client: Received {index} from transport"),
            Err(err) => {
                println!("client: Error during recv from transport: {err}");
                break;
            }
        }
        println!();
    }
}

fn main() {
    let (client_transport_tx, client_transport_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel();
    let (transport_client_tx, transport_client_rx): (Sender<PingReqResp>, Receiver<PingReqResp>) = mpsc::channel();

    let client = thread::spawn(move || client_thread(client_transport_tx, transport_client_rx));
    let tx_transport = thread::spawn(move || tx_transport_thread(client_transport_rx));
    let rx_transport = thread::spawn(move || rx_transport_thread(transport_client_tx));

    client.join().expect("oops, client crashed");
    tx_transport.join().expect("oops, tx transport crashed");
    rx_transport.join().expect("oops, rx transport crashed");
}
