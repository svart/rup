use std::io;
use std::net::SocketAddr;

use socket2::{SockRef, Socket};
use tokio::net::{TcpSocket, UdpSocket};

use crate::TrafficClass;

pub fn set_socket_tos(socket: &Socket, addr: SocketAddr, tos: TrafficClass) -> io::Result<()> {
    if addr.is_ipv4() {
        socket.set_tos_v4(tos.as_u8() as u32)
    } else {
        socket.set_tclass_v6(tos.as_u8() as u32)
    }
}

pub fn enable_socket_recv_tos(socket: &Socket, addr: SocketAddr) -> io::Result<()> {
    if addr.is_ipv4() {
        socket.set_recv_tos_v4(true)
    } else {
        socket.set_recv_tclass_v6(true)
    }
}

pub fn set_udp_tos(socket: &UdpSocket, addr: SocketAddr, tos: TrafficClass) -> io::Result<()> {
    let socket = SockRef::from(socket);
    if addr.is_ipv4() {
        socket.set_tos_v4(tos.as_u8() as u32)
    } else {
        socket.set_tclass_v6(tos.as_u8() as u32)
    }
}

pub fn set_tcp_tos(socket: &TcpSocket, addr: SocketAddr, tos: TrafficClass) -> io::Result<()> {
    let socket = SockRef::from(socket);
    if addr.is_ipv4() {
        socket.set_tos_v4(tos.as_u8() as u32)
    } else {
        socket.set_tclass_v6(tos.as_u8() as u32)
    }
}

#[cfg(unix)]
pub async fn recv_from_with_tos(
    socket: &UdpSocket,
    buf: &mut [u8],
) -> io::Result<(usize, SocketAddr, Option<TrafficClass>)> {
    use std::mem;
    use std::os::fd::AsRawFd;

    use tokio::io::Interest;

    loop {
        socket.readable().await?;

        let fd = socket.as_raw_fd();
        match socket.try_io(Interest::READABLE, || {
            let mut storage: libc::sockaddr_storage = unsafe { mem::zeroed() };
            let mut control = [0u8; 128];
            let mut iov = libc::iovec {
                iov_base: buf.as_mut_ptr().cast(),
                iov_len: buf.len(),
            };
            let mut msg: libc::msghdr = unsafe { mem::zeroed() };
            msg.msg_name = (&mut storage as *mut libc::sockaddr_storage).cast();
            msg.msg_namelen = mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = control.len();

            let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }

            let addr = socket_addr_from_storage(&storage, msg.msg_namelen)?;
            let tos = unsafe { parse_tos_cmsg(&msg) };
            Ok((n as usize, addr, tos))
        }) {
            Ok(result) => return Ok(result),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(not(unix))]
pub async fn recv_from_with_tos(
    socket: &UdpSocket,
    buf: &mut [u8],
) -> io::Result<(usize, SocketAddr, Option<TrafficClass>)> {
    let (n, addr) = socket.recv_from(buf).await?;
    Ok((n, addr, None))
}

#[cfg(unix)]
fn socket_addr_from_storage(
    storage: &libc::sockaddr_storage,
    len: libc::socklen_t,
) -> io::Result<SocketAddr> {
    use std::mem;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

    match storage.ss_family as libc::c_int {
        libc::AF_INET if len as usize >= mem::size_of::<libc::sockaddr_in>() => {
            let addr = unsafe { *(storage as *const _ as *const libc::sockaddr_in) };
            Ok(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::from(u32::from_be(addr.sin_addr.s_addr)),
                u16::from_be(addr.sin_port),
            )))
        }
        libc::AF_INET6 if len as usize >= mem::size_of::<libc::sockaddr_in6>() => {
            let addr = unsafe { *(storage as *const _ as *const libc::sockaddr_in6) };
            Ok(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(addr.sin6_addr.s6_addr),
                u16::from_be(addr.sin6_port),
                addr.sin6_flowinfo,
                addr.sin6_scope_id,
            )))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recvmsg returned an unsupported socket address",
        )),
    }
}

#[cfg(unix)]
unsafe fn parse_tos_cmsg(msg: &libc::msghdr) -> Option<TrafficClass> {
    let mut cmsg = unsafe { libc::CMSG_FIRSTHDR(msg) };
    while !cmsg.is_null() {
        let level = unsafe { (*cmsg).cmsg_level };
        let ty = unsafe { (*cmsg).cmsg_type };
        if (level == libc::IPPROTO_IP && ty == libc::IP_TOS)
            || (level == libc::IPPROTO_IPV6 && ty == libc::IPV6_TCLASS)
        {
            let data = unsafe { libc::CMSG_DATA(cmsg) };
            let data_len = unsafe { (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize };
            if data_len >= std::mem::size_of::<libc::c_int>() {
                let value = unsafe { std::ptr::read_unaligned(data.cast::<libc::c_int>()) };
                return Some(TrafficClass::new(value as u8));
            }
            if data_len >= 1 {
                return Some(TrafficClass::new(unsafe { *data }));
            }
        }
        cmsg = unsafe { libc::CMSG_NXTHDR(msg, cmsg) };
    }
    None
}
