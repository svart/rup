use std::io;
use std::time::Instant;

use crate::pinger::{Echo, PING_HDR_LEN, Request, Response};

pub fn encode_request(req: &Request) -> io::Result<Vec<u8>> {
    let echo = Echo {
        id: req.id,
        len: req.request_size.unwrap_or(PING_HDR_LEN as u16),
        resp_size: req.response_size.unwrap_or(0),
    };

    Ok(encode_echo(&echo, echo.len as usize))
}

pub fn decode_header(buf: &[u8]) -> io::Result<Echo> {
    if buf.len() < PING_HDR_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("packet too short: {} bytes", buf.len()),
        ));
    }

    let mut id = [0; 8];
    id.copy_from_slice(&buf[0..8]);
    let mut len = [0; 2];
    len.copy_from_slice(&buf[8..10]);
    let mut resp_size = [0; 2];
    resp_size.copy_from_slice(&buf[10..PING_HDR_LEN]);

    Ok(Echo {
        id: u64::from_le_bytes(id),
        len: u16::from_le_bytes(len),
        resp_size: u16::from_le_bytes(resp_size),
    })
}

pub fn decode_response(buf: &[u8]) -> io::Result<Response> {
    let echo = decode_header(buf)?;
    Ok(Response {
        id: echo.id,
        timestamp: Instant::now(),
        size: buf.len(),
        ttl: None,
    })
}

pub fn encode_response(mut echo: Echo) -> io::Result<Vec<u8>> {
    if echo.resp_size > 0 {
        echo.len = echo.resp_size;
    }
    echo.resp_size = 0;
    Ok(encode_echo(&echo, echo.len as usize))
}

pub fn encode_echo(echo: &Echo, len: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(len.max(PING_HDR_LEN));
    buf.extend_from_slice(&echo.id.to_le_bytes());
    buf.extend_from_slice(&echo.len.to_le_bytes());
    buf.extend_from_slice(&echo.resp_size.to_le_bytes());
    buf.resize(len, 0);
    buf
}
