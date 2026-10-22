use std::io;
use std::time::Instant;

use crate::pinger::{Echo, PING_HDR_LEN, PacketSize, Request, Response};

pub fn encode_request(req: &Request) -> Vec<u8> {
    let echo = Echo {
        id: req.id,
        len: req
            .request_size
            .map(PacketSize::get)
            .unwrap_or(PING_HDR_LEN as u16),
        resp_size: req.response_size.map(PacketSize::get).unwrap_or(0),
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

pub fn encode_response(mut echo: Echo) -> Vec<u8> {
    if echo.resp_size > 0 {
        echo.len = echo.resp_size;
    }
    echo.resp_size = 0;
    encode_echo(&echo, echo.len as usize)
}

pub fn encode_echo(echo: &Echo, len: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(len.max(PING_HDR_LEN));
    buf.extend_from_slice(&echo.id.to_le_bytes());
    buf.extend_from_slice(&echo.len.to_le_bytes());
    buf.extend_from_slice(&echo.resp_size.to_le_bytes());
    buf.resize(len, 0);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: u64, request_size: Option<u16>, response_size: Option<u16>) -> Request {
        Request {
            id,
            request_size: request_size.map(|size| PacketSize::new(size).unwrap()),
            response_size: response_size.map(|size| PacketSize::new(size).unwrap()),
        }
    }

    #[test]
    fn header_is_twelve_little_endian_bytes_in_field_order() {
        let echo = Echo {
            id: 1,
            len: 2,
            resp_size: 3,
        };
        let bytes = encode_echo(&echo, PING_HDR_LEN);

        assert_eq!(PING_HDR_LEN, 12);
        assert_eq!(bytes.len(), PING_HDR_LEN);
        assert_eq!(bytes[0..8], 1u64.to_le_bytes());
        assert_eq!(bytes[8..10], 2u16.to_le_bytes());
        assert_eq!(bytes[10..12], 3u16.to_le_bytes());
    }

    #[test]
    fn header_roundtrips_extreme_values() {
        for (id, value) in [(0, 0), (u64::MAX, u16::MAX)] {
            let echo = Echo {
                id,
                len: value,
                resp_size: value,
            };
            let decoded = decode_header(&encode_echo(&echo, PING_HDR_LEN)).unwrap();

            assert_eq!(decoded.id, id);
            assert_eq!(decoded.len, value);
            assert_eq!(decoded.resp_size, value);
        }
    }

    #[test]
    fn decode_header_rejects_short_buffers() {
        for len in [0, 4, PING_HDR_LEN - 1] {
            assert!(
                matches!(decode_header(&vec![0; len]), Err(e) if e.kind() == io::ErrorKind::InvalidData),
                "len={len}"
            );
        }
    }

    #[test]
    fn decode_header_ignores_trailing_bytes() {
        let mut bytes = encode_echo(
            &Echo {
                id: 9,
                len: 20,
                resp_size: 0,
            },
            PING_HDR_LEN,
        );
        bytes.extend_from_slice(&[0xff; 8]);

        let echo = decode_header(&bytes).unwrap();
        assert_eq!(echo.id, 9);
        assert_eq!(echo.len, 20);
    }

    #[test]
    fn encode_request_defaults_to_bare_header() {
        let bytes = encode_request(&request(10, None, None));
        let echo = decode_header(&bytes).unwrap();

        assert_eq!(bytes.len(), PING_HDR_LEN);
        assert_eq!(echo.id, 10);
        assert_eq!(echo.len, PING_HDR_LEN as u16);
        assert_eq!(echo.resp_size, 0);
    }

    #[test]
    fn encode_request_pads_with_zeros_and_carries_response_size() {
        let bytes = encode_request(&request(7, Some(64), Some(128)));
        let echo = decode_header(&bytes).unwrap();

        assert_eq!(bytes.len(), 64);
        assert_eq!(&bytes[PING_HDR_LEN..], &[0; 64 - PING_HDR_LEN]);
        assert_eq!(echo.id, 7);
        assert_eq!(echo.len, 64);
        assert_eq!(echo.resp_size, 128);
    }

    #[test]
    fn decode_response_reports_id_and_datagram_size() {
        let response = decode_response(&encode_request(&request(99, Some(40), None))).unwrap();

        assert_eq!(response.id, 99);
        assert_eq!(response.size, 40);
        assert_eq!(response.ttl, None);
    }
}
