// SPDX-License-Identifier: Apache-2.0
//! Socket helpers: exclusive binding and the OS view of this process's listening sockets.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::windows::io::RawSocket;

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, SO_EXCLUSIVEADDRUSE, SOCKET, SOL_SOCKET, WSAGetLastError, setsockopt,
};

/// Sets `SO_EXCLUSIVEADDRUSE` so no other process (even with `SO_REUSEADDR`) can bind the same
/// address and port. Must be called before `bind`.
pub fn set_exclusive_address_use(socket: RawSocket) -> io::Result<()> {
    let on: i32 = 1;
    // SAFETY: `socket` is a valid socket owned by the caller; the option value is a live i32.
    let rc = unsafe {
        setsockopt(
            socket as SOCKET,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&raw const on).cast(),
            std::mem::size_of::<i32>() as i32,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        // SAFETY: plain thread-local error query.
        Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
    }
}

fn tcp_table(family: u16) -> io::Result<Vec<u8>> {
    let mut size: u32 = 0;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..8 {
        // SAFETY: `buf` holds `size` bytes (or is empty with size 0 for the size query).
        let rc = unsafe {
            GetExtendedTcpTable(
                if buf.is_empty() {
                    std::ptr::null_mut()
                } else {
                    buf.as_mut_ptr().cast()
                },
                &mut size,
                0,
                u32::from(family),
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if rc == NO_ERROR {
            let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).collect();
            return Ok(bytes);
        }
        if rc != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        buf = vec![0u64; (size as usize).div_ceil(8) + 8];
        size = (buf.len() * 8) as u32;
    }
    Err(io::Error::other("GetExtendedTcpTable kept growing"))
}

fn port_from(dw: u32) -> u16 {
    // The low 16 bits hold the port in network byte order.
    u16::from_be(dw as u16)
}

/// Every TCP socket in the LISTEN state owned by `pid`, as the OS reports it.
pub fn listening_sockets_of(pid: u32) -> io::Result<Vec<SocketAddr>> {
    let mut out = Vec::new();

    let v4 = tcp_table(AF_INET)?;
    let n = u32::from_ne_bytes(v4[0..4].try_into().expect("4 bytes")) as usize;
    let row = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
    for i in 0..n {
        let off = 4 + i * row;
        let Some(bytes) = v4.get(off..off + row) else {
            break;
        };
        // SAFETY: `bytes` is exactly one row; read_unaligned copes with any alignment.
        let r: MIB_TCPROW_OWNER_PID = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast()) };
        if r.dwOwningPid == pid {
            let ip = Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes());
            out.push(SocketAddr::new(IpAddr::V4(ip), port_from(r.dwLocalPort)));
        }
    }

    let v6 = tcp_table(AF_INET6)?;
    let n = u32::from_ne_bytes(v6[0..4].try_into().expect("4 bytes")) as usize;
    let row = std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>();
    for i in 0..n {
        let off = 4 + i * row;
        let Some(bytes) = v6.get(off..off + row) else {
            break;
        };
        // SAFETY: as above.
        let r: MIB_TCP6ROW_OWNER_PID = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast()) };
        if r.dwOwningPid == pid {
            let ip = Ipv6Addr::from(r.ucLocalAddr);
            out.push(SocketAddr::new(IpAddr::V6(ip), port_from(r.dwLocalPort)));
        }
    }
    Ok(out)
}

/// [`listening_sockets_of`] for the current process.
pub fn listening_sockets_of_current_process() -> io::Result<Vec<SocketAddr>> {
    listening_sockets_of(std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sees_own_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let mine = listening_sockets_of_current_process().unwrap();
        assert!(mine.contains(&addr), "{mine:?} lacks {addr}");
        assert!(mine.iter().all(|a| !a.ip().is_unspecified()));
    }
}
