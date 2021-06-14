use std::io::Result;
use std::time::Instant;

pub const PING_MSG_LEN: usize = 8;

pub trait Pinger {
    fn send_req(&mut self) -> Result<Instant>;
    fn recv_resp(&mut self, last_send: Instant) -> Result<Vec<u128>>;
    fn send_err_handler(&self, err: std::io::Error) -> Result<()>;
}

