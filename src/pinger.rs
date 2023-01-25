use std::time::{Instant, Duration};

use tokio::{sync::mpsc::Sender, time};

pub const PING_MSG_LEN: usize = 8;

#[derive(Debug)]
pub(crate) enum MsgType {
    REQUEST,
    RESPONSE,
}

#[derive(Debug)]
pub(crate) struct PingReqResp {
    pub(crate) index: u64,
    pub(crate) timestamp: Instant,
    pub(crate) t: MsgType,
}

pub(crate) async fn generator(to_tx_transport: Sender<PingReqResp>,
                              interval: u64) {
    let mut i: u64 = 0;
    loop {
        let req = PingReqResp {
            index: i,
            timestamp: Instant::now(),
            t: MsgType::REQUEST
        };

        if let Err(err) = to_tx_transport.send(req).await {
            panic!("client: Error during sending {i} to transport: {err}");
        }

        time::sleep(Duration::from_millis(interval)).await;
        i += 1;
    }
}
