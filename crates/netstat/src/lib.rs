//! Windows open-port tables, the same thing `netstat -ano` shows:
//! which process (PID) holds which port.
//!
//! Read-only: the functions open nothing (no firewall prompt) and do not need
//! administrator rights. On other OSes the tables are empty: the program targets Windows.

pub mod process;

use std::io;

/// ID of the process holding a UDP port on any address (`0.0.0.0`, `127.0.0.1`,
/// `::`, `::1`…). `None` means the port is free. If several processes hold the port,
/// any one of them (enough for the "is the port free" question).
pub fn udp_port_owner(port: u16) -> io::Result<Option<u32>> {
    Ok(udp_port_owners(port)?.first().copied())
}

/// **All** processes with a UDP socket on this port (ascending, without duplicates).
///
/// On Windows different programs can hold the same port number on different addresses
/// (for example, the game on `0.0.0.0:P` and another program on `127.0.0.1:P`; packets to
/// `127.0.0.1:P` go to the second one). So "the port belongs to the game" only if
/// this list contains just the game.
pub fn udp_port_owners(port: u16) -> io::Result<Vec<u32>> {
    Ok(owners_of(&udp_sockets()?, port))
}

/// UDP ports of process `pid`, ascending, without duplicates.
pub fn udp_ports_of(pid: u32) -> io::Result<Vec<u16>> {
    let mut ports: Vec<u16> = udp_sockets()?
        .into_iter()
        .filter(|s| s.pid == pid)
        .map(|s| s.port)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

/// The process **listening** on a TCP port (waiting for connections). `None` means nobody
/// listens on it **or** several different processes do (then we cannot tell which of
/// them answered a connection to `127.0.0.1`).
pub fn tcp_listener_owner(port: u16) -> io::Result<Option<u32>> {
    match owners_of(&tcp_listeners()?, port).as_slice() {
        [single] => Ok(Some(*single)),
        _ => Ok(None),
    }
}

/// Port owners in a table snapshot: ascending, without duplicates.
pub fn owners_of(sockets: &[PortOwner], port: u16) -> Vec<u32> {
    let mut pids: Vec<u32> = sockets
        .iter()
        .filter(|s| s.port == port)
        .map(|s| s.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}
/// One open socket: port and owning process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortOwner {
    pub port: u16,
    pub pid: u32,
}

/// All UDP sockets on this PC (IPv4 and IPv6).
#[cfg(windows)]
pub fn udp_sockets() -> io::Result<Vec<PortOwner>> {
    use windows_sys::Win32::{
        NetworkManagement::IpHelper::{
            GetExtendedUdpTable, MIB_UDP6ROW_OWNER_PID, MIB_UDP6TABLE_OWNER_PID,
            MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, UDP_TABLE_OWNER_PID,
        },
        Networking::WinSock::{AF_INET, AF_INET6},
    };

    let fetch = |buf: *mut core::ffi::c_void, size: *mut u32, family: u32| {
        // SAFETY: the caller (`table`) passes the buffer and its real size.
        unsafe { GetExtendedUdpTable(buf, size, 0, family, UDP_TABLE_OWNER_PID, 0) }
    };
    let mut out = Vec::new();
    let v4 = table(u32::from(AF_INET), fetch)?;
    out.extend(
        rows::<MIB_UDPROW_OWNER_PID>(&v4, std::mem::offset_of!(MIB_UDPTABLE_OWNER_PID, table))
            .into_iter()
            .map(|r| PortOwner {
                port: port_from_raw(r.dwLocalPort),
                pid: r.dwOwningPid,
            }),
    );
    let v6 = table(u32::from(AF_INET6), fetch)?;
    out.extend(
        rows::<MIB_UDP6ROW_OWNER_PID>(&v6, std::mem::offset_of!(MIB_UDP6TABLE_OWNER_PID, table))
            .into_iter()
            .map(|r| PortOwner {
                port: port_from_raw(r.dwLocalPort),
                pid: r.dwOwningPid,
            }),
    );
    Ok(out)
}

/// All TCP sockets on this PC in the "listening" state (IPv4 and IPv6).
#[cfg(windows)]
pub fn tcp_listeners() -> io::Result<Vec<PortOwner>> {
    use windows_sys::Win32::{
        NetworkManagement::IpHelper::{
            GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
            MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
        },
        Networking::WinSock::{AF_INET, AF_INET6},
    };

    let fetch = |buf: *mut core::ffi::c_void, size: *mut u32, family: u32| {
        // SAFETY: the caller (`table`) passes the buffer and its real size.
        unsafe { GetExtendedTcpTable(buf, size, 0, family, TCP_TABLE_OWNER_PID_LISTENER, 0) }
    };
    let mut out = Vec::new();
    let v4 = table(u32::from(AF_INET), fetch)?;
    out.extend(
        rows::<MIB_TCPROW_OWNER_PID>(&v4, std::mem::offset_of!(MIB_TCPTABLE_OWNER_PID, table))
            .into_iter()
            .map(|r| PortOwner {
                port: port_from_raw(r.dwLocalPort),
                pid: r.dwOwningPid,
            }),
    );
    let v6 = table(u32::from(AF_INET6), fetch)?;
    out.extend(
        rows::<MIB_TCP6ROW_OWNER_PID>(&v6, std::mem::offset_of!(MIB_TCP6TABLE_OWNER_PID, table))
            .into_iter()
            .map(|r| PortOwner {
                port: port_from_raw(r.dwLocalPort),
                pid: r.dwOwningPid,
            }),
    );
    Ok(out)
}

#[cfg(not(windows))]
pub fn udp_sockets() -> io::Result<Vec<PortOwner>> {
    Ok(Vec::new())
}

#[cfg(not(windows))]
pub fn tcp_listeners() -> io::Result<Vec<PortOwner>> {
    Ok(Vec::new())
}

/// The table stores the port in the low 16 bits in network byte order.
#[cfg(windows)]
fn port_from_raw(raw: u32) -> u16 {
    u16::from_be(raw as u16)
}

/// Raw table bytes. `fetch(buffer, size, family)` is `GetExtendedUdpTable` or
/// `GetExtendedTcpTable`.
#[cfg(windows)]
fn table(
    family: u32,
    fetch: impl Fn(*mut core::ffi::c_void, *mut u32, u32) -> u32,
) -> io::Result<Vec<u8>> {
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

    let mut size: u32 = 0;
    // The table can grow between calls, so try several times.
    for _ in 0..5 {
        // u32 buffer: this aligns the table start to 4 bytes, as Windows expects.
        let mut buf = vec![0u32; (size as usize + 1024).div_ceil(4)];
        size = (buf.len() * 4) as u32;
        match fetch(buf.as_mut_ptr().cast(), &mut size, family) {
            0 => {
                let bytes = buf.iter().flat_map(|w| w.to_ne_bytes()).collect();
                return Ok(bytes);
            }
            ERROR_INSUFFICIENT_BUFFER => continue,
            code => return Err(io::Error::from_raw_os_error(code as i32)),
        }
    }
    Err(io::Error::other(phase_lang::tr!(
        "таблица портов постоянно меняется",
        "the port table keeps changing"
    )))
}

/// Table rows: first a `u32` row count, then the rows themselves from offset `rows_offset`.
#[cfg(windows)]
fn rows<T: Copy>(buf: &[u8], rows_offset: usize) -> Vec<T> {
    if buf.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let row_size = std::mem::size_of::<T>();
    (0..count)
        .map_while(|i| {
            let start = rows_offset + i * row_size;
            let bytes = buf.get(start..start + row_size)?;
            // SAFETY: the slice is exactly the size of T; T is a plain struct of numbers (repr(C)),
            // read without alignment requirements.
            Some(unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) })
        })
        .collect()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, TcpListener, UdpSocket};

    // Tests open sockets on 127.0.0.1 only, so there is no firewall prompt.

    #[test]
    fn finds_udp_owner_and_ports_of_process() {
        let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let (pa, pb) = (
            a.local_addr().unwrap().port(),
            b.local_addr().unwrap().port(),
        );
        let me = std::process::id();
        assert_eq!(udp_port_owner(pa).unwrap(), Some(me));
        let mine = udp_ports_of(me).unwrap();
        assert!(mine.contains(&pa) && mine.contains(&pb), "{mine:?}");
        drop(a);
        assert_eq!(udp_port_owner(pa).unwrap(), None);
    }

    #[test]
    fn owners_of_lists_every_process_once() {
        let table = [
            PortOwner { port: 5000, pid: 7 },
            PortOwner { port: 5000, pid: 3 },
            PortOwner { port: 5000, pid: 7 },
            PortOwner { port: 6000, pid: 9 },
        ];
        assert_eq!(owners_of(&table, 5000), vec![3, 7]);
        assert_eq!(owners_of(&table, 6000), vec![9]);
        assert!(owners_of(&table, 7000).is_empty());
    }

    #[test]
    fn finds_tcp_listener_owner() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert_eq!(tcp_listener_owner(port).unwrap(), Some(std::process::id()));
        drop(listener);
        assert_eq!(tcp_listener_owner(port).unwrap(), None);
    }
}
