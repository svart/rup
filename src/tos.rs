use std::io;
use std::net::SocketAddr;

use socket2::{SockRef, Socket};
use tokio::net::{TcpSocket, UdpSocket};

use crate::TrafficClass;

pub fn set_socket_tos(socket: &Socket, addr: SocketAddr, tos: TrafficClass) -> io::Result<()> {
    set_sockref_tos(SockRef::from(socket), addr, tos)
}

pub fn enable_socket_recv_tos(socket: &Socket, addr: SocketAddr) -> io::Result<()> {
    if addr.is_ipv4() {
        socket.set_recv_tos_v4(true)
    } else {
        socket.set_recv_tclass_v6(true)
    }
}

pub fn set_udp_tos(socket: &UdpSocket, addr: SocketAddr, tos: TrafficClass) -> io::Result<()> {
    set_sockref_tos(SockRef::from(socket), addr, tos)
}

pub fn set_tcp_tos(socket: &TcpSocket, addr: SocketAddr, tos: TrafficClass) -> io::Result<()> {
    set_sockref_tos(SockRef::from(socket), addr, tos)
}

fn set_sockref_tos(socket: SockRef<'_>, addr: SocketAddr, tos: TrafficClass) -> io::Result<()> {
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
            msg.msg_controllen = control.len() as _;

            let n = unsafe { libc::recvmsg(fd, &mut msg, 0) };
            if n < 0 {
                return Err(io::Error::last_os_error());
            }

            let addr = crate::unix::socket_address(&storage, msg.msg_namelen)?;
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
unsafe fn parse_tos_cmsg(msg: &libc::msghdr) -> Option<TrafficClass> {
    let data = unsafe {
        crate::unix::control_message_data(msg, libc::IPPROTO_IP, libc::IP_TOS).or_else(|| {
            crate::unix::control_message_data(msg, libc::IPPROTO_IPV6, libc::IPV6_TCLASS)
        })
    }?;
    if data.len() >= std::mem::size_of::<libc::c_int>() {
        let value = unsafe { std::ptr::read_unaligned(data.as_ptr().cast::<libc::c_int>()) };
        return Some(TrafficClass::new(value as u8));
    }
    data.first().copied().map(TrafficClass::new)
}
