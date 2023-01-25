use std::net::SocketAddr;
use std::time::Instant;

use tokio::sync::mpsc::{Sender, Receiver};
use tokio::net::{TcpListener, TcpStream, TcpSocket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::pinger::{MsgType, PingReqResp, PING_MSG_LEN};

async fn server_connection_handler(mut sock:TcpStream) {
    let peer_addr = sock.peer_addr().unwrap();
    println!("New connection from {peer_addr}");
    loop {
        let mut buf = [0; PING_MSG_LEN];

        match sock.read(&mut buf).await {
            Ok(0) => {
                println!("Connection closed: {peer_addr}", );
                break;
            }
            Ok(_) => {
                match sock.write_all(&buf).await {
                    Err(e) => {
                        println!("An error occurred during writing echo, \
                                  terminating connection with {}: {}",
                                 peer_addr, e);
                    },
                    _ => continue,
                }
            },
            Err(_) => {
                println!("An error occurred during reading request, \
                          terminating connection with {}", peer_addr);
                break;
            }
        }
    }
}

pub(crate) async fn server_transport(local_address: SocketAddr) {
    println!("Running TCP server listening {local_address}");
    let listen_sock = TcpListener::bind(local_address).await.expect("server: binding failed");

    loop {
        match listen_sock.accept().await {
            Ok((socket, _)) => {
                tokio::spawn(server_connection_handler(socket));
            },
            Err(e) => println!("Connection failed: {e}"),
        }
    }
}

pub(crate) async fn pinger_transport(mut from_client: Receiver<PingReqResp>,
                                     to_client: Sender<PingReqResp>,
                                     local_address: SocketAddr,
                                     remote_address: SocketAddr) {
    let sock = TcpSocket::new_v4().unwrap();
    sock.bind(local_address).expect("pinger: bind failed");
    let mut sock = sock.connect(remote_address).await.expect("pinger: connection failed");

    loop {
        let mut buf = [0; 8];

        tokio::select! {
            r_val = from_client.recv() => {
                match r_val {
                    Some(mut req) => {
                        // Sending request to socket
                        let index = req.index;
                        req.timestamp = Instant::now();

                        sock.write_all(&index.to_be_bytes()).await.expect("tx: couldn't send message");

                        to_client.send(req).await.expect("tx: couldn't send transformed request to client");
                    }
                    None => break,
                }
            }
            r_val = sock.read(&mut buf) => {
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
