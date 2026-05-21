use std::io;
use std::time::Instant;

use crate::pinger::{Echo, PING_HDR_LEN, Request, Response};

pub fn encode_request(req: &Request) -> io::Result<Vec<u8>> {
    let echo = Echo {
        id: req.id,
        len: req.request_size.unwrap_or(PING_HDR_LEN as u16),
        resp_size: req.response_size.unwrap_or(0),
    };

    encode_echo(&echo, echo.len as usize)
}

pub fn decode_header(buf: &[u8]) -> io::Result<Echo> {
    if buf.len() < PING_HDR_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("packet too short: {} bytes", buf.len()),
        ));
    }

    bincode::deserialize(&buf[..PING_HDR_LEN])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("deserialize: {e}")))
}

pub fn decode_response(buf: &[u8]) -> io::Result<Response> {
    let echo = decode_header(buf)?;
    Ok(Response {
        id: echo.id,
        timestamp: Instant::now(),
    })
}

pub fn encode_response(mut echo: Echo) -> io::Result<Vec<u8>> {
    if echo.resp_size > 0 {
        echo.len = echo.resp_size;
    }
    echo.resp_size = 0;
    encode_echo(&echo, echo.len as usize)
}

pub fn encode_echo(echo: &Echo, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = bincode::serialize(echo)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize: {e}")))?;
    buf.resize(len, 0);
    Ok(buf)
}
