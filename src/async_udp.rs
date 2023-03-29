use std::net::SocketAddr;
use std::collections::HashSet;
use std::time::Instant;

use tokio::sync::mpsc::{Receiver, Sender};
use tokio::net::UdpSocket;

use crate::pinger::{MsgType, PingReqResp, PING_HDR_LEN, Echo};

pub(crate) async fn server_transport(local_address: SocketAddr) {
    let sock = UdpSocket::bind(local_address).await.expect("server: binding failed");

    let mut client_addrs: HashSet<SocketAddr> = HashSet::new();

    let mut buf = [0; u16::MAX as usize];

    loop {
        let (_, addr) = sock.recv_from(&mut buf).await.unwrap();

        if !client_addrs.contains(&addr) {
            println!("New UDP request from {addr}");
            client_addrs.insert(addr);
        }

        let mut req: Echo = bincode::deserialize(&buf[..PING_HDR_LEN]).unwrap();
        req.len = req.resp_size;
        req.resp_size = 0;

        let mut send_buf = bincode::serialize(&req).unwrap();

        send_buf.resize(req.len as usize, 0);

        sock.send_to(&send_buf, addr).await.unwrap();
    }
}

pub(crate) async fn pinger_transport(
    mut from_generator: Receiver<PingReqResp>,
    to_statista: Sender<PingReqResp>,
    local_address: SocketAddr,
    remote_address: SocketAddr,
    request_size: Option<u16>,
    response_size: Option<u16>
) {
    let sock = UdpSocket::bind(local_address).await.expect("pinger: binding failed");
    sock.connect(remote_address).await.expect("pinger: connect function failed");

    let mut buf = [0; u16::MAX as usize];

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

                        sock.send(&send_buf).await.expect("tx: couldn't send message");

                        to_statista.send(req).await.expect("tx: couldn't send transformed request to client");
                    }
                    None => break,
                }
            }
            _ = sock.recv(&mut buf) => {
                let p_resp: Echo = bincode::deserialize(&buf[..PING_HDR_LEN]).unwrap();
                let req = PingReqResp {
                    index: p_resp.id,
                    timestamp: Instant::now(),
                    t: MsgType::Response,
                };
                to_statista.send(req).await.unwrap();
            }
        }
    }
}
