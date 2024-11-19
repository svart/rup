use std::{net::SocketAddr, time::Instant};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::pinger::{Echo, MsgType, PingReqResp, PING_HDR_LEN};

const IP_HEADER_LEN: usize = 20;
const ICMP_HEADER_LEN: usize = 8;

pub(crate) async fn pinger_transport(
    mut from_generator: Receiver<PingReqResp>,
    to_statista: Sender<PingReqResp>,
    _local_address: SocketAddr,
    mut remote_address: SocketAddr,
    request_size: Option<u16>,
    response_size: Option<u16>,
) {
    let sock = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4)).expect("should be able to create socket");
    sock.set_nonblocking(true).expect("should be able to set nonblocking for socket");
    let sock = tokio::net::UdpSocket::from_std(sock.into()).expect("should be able to create async socket from fd");

    remote_address.set_port(0);
    sock.connect(remote_address).await.expect("pinger: should be able to connect socket");

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

                        let icmp_header = vec![
                            0x08, 0x00,   // Type, Code: Echo request
                            0x00, 0x00,   // Checksum placeholder
                            0x12, 0x34,   // Identifier
                            (index >> 8) as u8, (index & 0xff) as u8,  // Sequence number
                        ];

                        let mut send_buf = bincode::serialize(&r).unwrap();

                        if let Some(size) = request_size {
                            send_buf.resize(size as usize + ICMP_HEADER_LEN, 0);
                        } else {
                            send_buf.resize(PING_HDR_LEN + ICMP_HEADER_LEN, 0);
                        }
                        let len = send_buf.len() - ICMP_HEADER_LEN;
                        send_buf.copy_within(0..len, ICMP_HEADER_LEN);
                        send_buf[..ICMP_HEADER_LEN].copy_from_slice(&icmp_header);

                        let checksum = csum16_slice(&send_buf);
                        send_buf[2] = (checksum >> 8) as u8;
                        send_buf[3] = (checksum & 0xff) as u8;

                        // println!("Buffer to send: {:02X?}", send_buf);

                        sock.send(&send_buf).await.expect("tx: should send to socket normally");
                        to_statista.send(req).await.expect("tx: should send request to stats normally");
                    }
                    None => break,
                }
            }
            r_val = sock.recv(&mut buf) => {
                if r_val.is_err() {
                    break;
                }

                // println!("Recv buffer: {:02X?}", &buf[IP_HEADER_LEN..r_val.unwrap()]);

                const DATA_OFFSET: usize = IP_HEADER_LEN + ICMP_HEADER_LEN;
                let p_resp: Echo = bincode::deserialize(&buf[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN]).unwrap();
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

fn csum16_add(x: u16, y: u16) -> u16 {
    let s = (x as u32) + (y as u32);

    if s & 0x1_00_00 > 0 {
        (s + 1) as u16
    } else {
        s as u16
    }
}

fn csum16_slice(data: &[u8]) -> u16 {
    assert!(data.len() %2 == 0);

    let mut csum = 0;
    for chunk in data.chunks_exact(2) {
        let hi = chunk[0] as u16;
        let lo = chunk[1] as u16;
        csum = csum16_add(csum, (hi << 8) | lo);
    }

    !csum
}
