use std::io;
use std::mem;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

pub(crate) unsafe fn control_message_data(
    message: &libc::msghdr,
    level: libc::c_int,
    kind: libc::c_int,
) -> Option<&[u8]> {
    let mut header = unsafe { libc::CMSG_FIRSTHDR(message) };
    while !header.is_null() {
        if unsafe { (*header).cmsg_level == level && (*header).cmsg_type == kind } {
            let header_len = unsafe { libc::CMSG_LEN(0) as usize };
            let data_len = unsafe { (*header).cmsg_len as usize }.saturating_sub(header_len);
            let data = unsafe { libc::CMSG_DATA(header) };
            return Some(unsafe { std::slice::from_raw_parts(data, data_len) });
        }
        header = unsafe { libc::CMSG_NXTHDR(message, header) };
    }
    None
}

pub(crate) fn socket_address(
    storage: &libc::sockaddr_storage,
    len: libc::socklen_t,
) -> io::Result<SocketAddr> {
    match storage.ss_family as libc::c_int {
        libc::AF_INET if len as usize >= mem::size_of::<libc::sockaddr_in>() => {
            let address = unsafe {
                std::ptr::read_unaligned(std::ptr::from_ref(storage).cast::<libc::sockaddr_in>())
            };
            Ok(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr)),
                u16::from_be(address.sin_port),
            )))
        }
        libc::AF_INET6 if len as usize >= mem::size_of::<libc::sockaddr_in6>() => {
            let address = unsafe {
                std::ptr::read_unaligned(std::ptr::from_ref(storage).cast::<libc::sockaddr_in6>())
            };
            Ok(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(address.sin6_addr.s6_addr),
                u16::from_be(address.sin6_port),
                address.sin6_flowinfo,
                address.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recvmsg returned an unsupported socket address",
        )),
    }
}
