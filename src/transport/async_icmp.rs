use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Instant,
    sync::Arc,
};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot;

use crate::pinger::{Echo, MsgType, PingReqResp, PING_HDR_LEN};

const IP_HEADER_LEN: usize = 20;
const ICMP_HEADER_LEN: usize = 8;


// TODO
// Transport API:
// - from generator (Receiver<PingReqResp>)
// - to statista (Sender<PingReqResp>)
// - stopper (broadcast)

pub(crate) async fn pinger_transport(
    from_generator: Receiver<PingReqResp>,
    to_statista: Sender<PingReqResp>,
    local_address: SocketAddr,
    mut remote_address: SocketAddr,
    request_size: Option<u16>,
    response_size: Option<u16>,
) {
    let sock = Socket::new(Domain::IPV4, Type::RAW, Some(Protocol::ICMPV4))
        .expect("should be able to create socket");
    sock.bind(&local_address.into())
        .expect("should be able to bind to local address");
    sock.set_nonblocking(true)
        .expect("should be able to set nonblocking for socket");
    let sock = tokio::net::UdpSocket::from_std(sock.into())
        .expect("should be able to create async socket from fd");

    remote_address.set_port(0);
    sock.connect(remote_address)
        .await
        .expect("pinger: should be able to connect socket");

    let sock = Arc::new(sock);

    let (stopper_tx, mut stopper_rx) = tokio::sync::oneshot::channel();

    let to_stat_copy = to_statista.clone();
    let sock_copy = sock.clone();
    tokio::spawn(sender(
        from_generator,
        to_stat_copy,
        sock_copy,
        request_size,
        response_size,
        stopper_tx,
    ));

    let mut buf = [0; u16::MAX as usize];

    loop {
        tokio::select! {
            r_val = sock.recv(&mut buf) => {
                match r_val {
                    Ok(_) => {
                        const DATA_OFFSET: usize = IP_HEADER_LEN + ICMP_HEADER_LEN;

                        // println!("RX IP:   {:02X?}", &buf[..IP_HEADER_LEN]);
                        // println!("RX ICMP: {:02X?}", &buf[IP_HEADER_LEN..IP_HEADER_LEN + ICMP_HEADER_LEN]);
                        // println!("RX DATA: {:02X?}", &buf[DATA_OFFSET..]);

                        if packet_is_good(&buf, &remote_address) {
                            let p_resp: Echo = bincode::deserialize(&buf[DATA_OFFSET..DATA_OFFSET + PING_HDR_LEN]).unwrap();
                            let req = PingReqResp {
                                index: p_resp.id,
                                timestamp: Instant::now(),
                                t: MsgType::Response,
                            };
                            to_statista.send(req).await.expect("transport rx: should send response to stats normally");
                        }
                    }
                    Err(e) => {
                        println!("transport rx: error reading from socket: {e}");
                        return;
                    }
                }
            }
            _ = &mut stopper_rx => {
                println!("transport rx: got stop signal, going out");
                return;
            }
        }
    }
}

async fn sender(
    mut from_generator: Receiver<PingReqResp>,
    to_statista: Sender<PingReqResp>,
    sock: Arc<UdpSocket>,
    request_size: Option<u16>,
    response_size: Option<u16>,
    stopper: oneshot::Sender<()>,
) {
    while let Some(mut req) = from_generator.recv().await {
        // Sending request to socket
        let index = req.index;

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

        // println!("TX ICMP: {:02X?}", &send_buf[..ICMP_HEADER_LEN]);
        // println!("TX DATA: {:02X?}", &send_buf[ICMP_HEADER_LEN..]);

        req.timestamp = Instant::now();
        sock.send(&send_buf).await.expect("tx: should send to socket normally");
        to_statista.send(req).await.expect("tx: should send request to stats normally");
        println!("transport: sent {index}");
    }

    println!("transport tx: all sent going out");
    stopper.send(()).expect("transport tx: should be able to send stop signal normally");
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
    assert!(data.len() % 2 == 0);

    let mut csum = 0;
    for chunk in data.chunks_exact(2) {
        let hi = chunk[0] as u16;
        let lo = chunk[1] as u16;
        csum = csum16_add(csum, (hi << 8) | lo);
    }

    !csum
}

fn packet_is_good(buf: &[u8], remote_address: &SocketAddr) -> bool {
    let ip = &buf[..IP_HEADER_LEN];
    let ip_addr = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
    if remote_address.ip() != ip_addr {
        return false;
    }

    let icmp = &buf[IP_HEADER_LEN..IP_HEADER_LEN + ICMP_HEADER_LEN];
    if icmp[0] != 0x00 || icmp[1] != 0x00 {
        return false;
    }
    if icmp[4] != 0x12 || icmp[5] != 0x34 {
        return false;
    }

    true
}
