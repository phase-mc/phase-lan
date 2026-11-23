//! Sockets with safe settings for Windows.

use std::{io, net::SocketAddr};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

/// UDP socket on `addr` with the `SO_EXCLUSIVEADDRUSE` flag: after us no other program
/// can take the same port and intercept our traffic.
///
/// Note: this flag does **not** check whether the port is already taken. On Windows a socket on `127.0.0.1:P`
/// opens even if another program listens on `0.0.0.0:P`. So before opening a
/// port whose number was not chosen by our computer, call [`udp_port_in_use`].
pub fn bind_udp_exclusive(addr: SocketAddr) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    #[cfg(windows)]
    set_exclusive_address_use(&socket)?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&addr.into())?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

#[cfg(windows)]
fn set_exclusive_address_use(socket: &Socket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;

    use windows_sys::Win32::Networking::WinSock::{SO_EXCLUSIVEADDRUSE, SOL_SOCKET, setsockopt};

    let enable: i32 = 1;
    // SAFETY: the socket is open and ours; we pass a pointer to an i32 and its size,
    // as setsockopt requires for SO_EXCLUSIVEADDRUSE.
    let rc = unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&enable as *const i32).cast::<u8>(),
            std::mem::size_of::<i32>() as i32,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Whether a UDP port is taken on this PC, on any address (`0.0.0.0`, `127.0.0.1`, `::`, `::1`…).
/// Returns the ID of the process (PID) that holds it.
///
/// Only reads the Windows port table: opens nothing, so there is no firewall prompt.
pub fn udp_port_in_use(port: u16) -> io::Result<Option<u32>> {
    phase_netstat::udp_port_owner(port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn exclusive_bind_refuses_busy_port() {
        let first = bind_udp_exclusive(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        let port = first.local_addr().unwrap().port();
        let second = bind_udp_exclusive(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
        assert!(second.is_err(), "a taken port must not open again");
    }

    #[cfg(windows)]
    #[test]
    fn detects_port_in_use() {
        // The test opens a socket on 127.0.0.1 only, so there is no firewall prompt.
        let other = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = other.local_addr().unwrap().port();
        let pid = udp_port_in_use(port).unwrap();
        assert_eq!(pid, Some(std::process::id()));
        drop(other);
        assert_eq!(udp_port_in_use(port).unwrap(), None);
    }
}
