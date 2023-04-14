use std::net::SocketAddr;
use std::time::Instant;

use tokio::sync::mpsc;
use tokio::net::{TcpListener, TcpStream, TcpSocket};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::pinger::{MsgType, PingReqResp, PING_HDR_LEN, Echo};

async fn server_connection_handler(mut sock:TcpStream) {
    let peer_addr = sock.peer_addr().unwrap();
    println!("New TCP connection from {peer_addr}");

    let mut hdr_buf = [0; PING_HDR_LEN];

    loop {
        match sock.read(&mut hdr_buf).await {
            Ok(0) => {
                println!("Connection closed: {peer_addr}", );
                break;
            }
            Ok(amt) => {
                let mut req: Echo = bincode::deserialize(&hdr_buf).unwrap();

                if req.len as usize > amt {
                    let mut for_read_buf = Vec::new();
                    for_read_buf.resize(req.len as usize - amt, 0);

                    match sock.read(&mut for_read_buf).await {
                        Ok(0) => {
                            println!("Connection closed: {peer_addr}");
                            break;
                        }
                        Ok(_) => { }
                        Err(_) => {
                            println!("An error occurred during reading request, \
                                    terminating connection with {peer_addr}");
                            break;
                        }
                    }
                }
                req.len = req.resp_size;
                req.resp_size = 0;

                let mut send_buf = bincode::serialize(&req).unwrap();
                send_buf.resize(req.len as usize, 0);

                match sock.write_all(&send_buf).await {
                    Err(e) => {
                        println!("An error occured during writing echo, \
                                    terminating connection with {peer_addr}: {e}");
                        break;
                    }
                    _ => continue
                }
            },
            Err(_) => {
                println!("An error occurred during reading request, \
                          terminating connection with {peer_addr}");
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

pub(crate) async fn pinger_transport(
    mut from_generator: mpsc::Receiver<PingReqResp>,
    to_statista: mpsc::Sender<PingReqResp>,
    local_address: SocketAddr,
    remote_address: SocketAddr,
    request_size: Option<u16>,
    response_size: Option<u16>
) {
    let sock = TcpSocket::new_v4().unwrap();
    sock.bind(local_address).expect("pinger: bind failed");
    let mut sock = sock.connect(remote_address).await.expect("pinger: connection failed");

    let mut buf = [0; PING_HDR_LEN];

    loop {
        tokio::select! {
            r_val = from_generator.recv() => {
                match r_val {
                    Some(mut req) => {
                        // Sending request to socket
                        let index = req.index;
                        req.timestamp = Instant::now();

                        let r = Echo {
                            id: index,
                            len: request_size.unwrap_or(PING_HDR_LEN as u16),
                            resp_size: response_size.unwrap_or(PING_HDR_LEN as u16),
                        };

                        let mut send_buf = bincode::serialize(&r).unwrap();

                        if let Some(size) = request_size {
                            send_buf.resize(size as usize, 0);
                        }

                        sock.write_all(&send_buf).await.expect("tx: couldn't send message");

                        to_statista.send(req).await.expect("tx: couldn't send transformed request to client");
                    }
                    None => break,
                }
            }
            r_val = sock.read(&mut buf) => {
                if r_val.is_err() {
                    break;
                }

                let p_resp: Echo = bincode::deserialize(&buf).unwrap();

                let req = PingReqResp {
                    index: p_resp.id,
                    timestamp: Instant::now(),
                    t: MsgType::Response
                };

                if p_resp.len as usize > PING_HDR_LEN {
                    let mut for_read_buf = Vec::new();
                    for_read_buf.resize(p_resp.len as usize - PING_HDR_LEN, 0);

                    match sock.read(&mut for_read_buf).await {
                        Ok(0) => {
                            panic!("Connection closed: {remote_address}");
                        }
                        Ok(_) => { }
                        Err(_) => {
                            panic!("An error occurred during reading response,\
                                    terminating connection with {remote_address}");
                        }
                    }
                }

                to_statista.send(req).await.unwrap();
            }
        }
    }
}
