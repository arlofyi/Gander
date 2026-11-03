// SPDX-License-Identifier: Apache-2.0
//! Interface enumeration (`GetAdaptersAddresses`) and address-change notifications
//! (`NotifyUnicastIpAddressChange`), feeding `gander_core::net::plan`.

use std::ffi::c_void;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use gander_core::net::plan::InterfaceInfo;
use windows_sys::Win32::Foundation::{
    ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, ERROR_SUCCESS, HANDLE, WIN32_ERROR,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH, MIB_NOTIFICATION_TYPE,
    MIB_UNICASTIPADDRESS_ROW, NotifyUnicastIpAddressChange,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6, AF_UNSPEC, SOCKET_ADDRESS};

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
/// `p` is null or points to a NUL-terminated UTF-16 string.
unsafe fn wide_to_string(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller guarantees a NUL terminator.
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

/// Reads a NUL-terminated ANSI string (adapter names are ASCII GUIDs).
///
/// # Safety
/// `p` is null or points to a NUL-terminated byte string.
unsafe fn ansi_to_string(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees a NUL terminator.
    unsafe {
        std::ffi::CStr::from_ptr(p.cast())
            .to_string_lossy()
            .into_owned()
    }
}

/// Converts a `SOCKET_ADDRESS` to an IP address.
///
/// # Safety
/// `sa.lpSockaddr` is null or points to `sa.iSockaddrLength` readable bytes.
unsafe fn socket_address_ip(sa: &SOCKET_ADDRESS) -> Option<IpAddr> {
    if sa.lpSockaddr.is_null() || sa.iSockaddrLength < 2 {
        return None;
    }
    let len = usize::try_from(sa.iSockaddrLength).ok()?;
    // SAFETY: the caller guarantees `len` readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(sa.lpSockaddr.cast::<u8>(), len) };
    let family = u16::from_ne_bytes([bytes[0], bytes[1]]);
    if family == AF_INET && len >= 8 {
        // SOCKADDR_IN: family(2) port(2) addr(4)
        Some(IpAddr::V4(Ipv4Addr::new(
            bytes[4], bytes[5], bytes[6], bytes[7],
        )))
    } else if family == AF_INET6 && len >= 24 {
        // SOCKADDR_IN6: family(2) port(2) flowinfo(4) addr(16)
        let mut a = [0u8; 16];
        a.copy_from_slice(&bytes[8..24]);
        Some(IpAddr::V6(Ipv6Addr::from(a)))
    } else {
        None
    }
}

/// Enumerates the machine's interfaces with their unicast addresses.
pub fn list_interfaces() -> io::Result<Vec<InterfaceInfo>> {
    let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
    let mut size: u32 = 16 * 1024;
    for _ in 0..4 {
        // u64 storage keeps the buffer aligned for IP_ADAPTER_ADDRESSES_LH.
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let first = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: `buf` holds `size` writable bytes; `size` is updated on overflow.
        let rc = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                flags,
                std::ptr::null(),
                first,
                &mut size,
            )
        };
        match rc {
            ERROR_SUCCESS => {
                let mut out = Vec::new();
                let mut cur = first.cast_const();
                while !cur.is_null() {
                    // SAFETY: the list lives inside `buf`, which outlives this loop; every
                    // pointer in it is null or valid as documented for GetAdaptersAddresses.
                    let a = unsafe { &*cur };
                    let mut addresses = Vec::new();
                    let mut u = a.FirstUnicastAddress.cast_const();
                    while !u.is_null() {
                        // SAFETY: as above.
                        let ua = unsafe { &*u };
                        // SAFETY: SOCKET_ADDRESS from the OS list.
                        if let Some(ip) = unsafe { socket_address_ip(&ua.Address) } {
                            addresses.push(ip);
                        }
                        u = ua.Next.cast_const();
                    }
                    // SAFETY: OS strings in the list, NUL-terminated or null.
                    let (guid, alias, description) = unsafe {
                        (
                            ansi_to_string(a.AdapterName),
                            wide_to_string(a.FriendlyName),
                            wide_to_string(a.Description),
                        )
                    };
                    out.push(InterfaceInfo {
                        guid: Some(guid).filter(|g| !g.is_empty()),
                        alias,
                        description,
                        up: a.OperStatus == IfOperStatusUp,
                        addresses,
                    });
                    cur = a.Next.cast_const();
                }
                return Ok(out);
            }
            ERROR_NO_DATA => return Ok(Vec::new()),
            ERROR_BUFFER_OVERFLOW => continue,
            other => return Err(io::Error::from_raw_os_error(other as i32)),
        }
    }
    Err(io::Error::other("GetAdaptersAddresses kept overflowing"))
}

type Callback = Arc<dyn Fn() + Send + Sync>;

unsafe extern "system" fn on_change(
    context: *const c_void,
    _row: *const MIB_UNICASTIPADDRESS_ROW,
    _kind: MIB_NOTIFICATION_TYPE,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: `context` is the `Box<Callback>` registered in `AddressChangeWatcher::new`, which
    // is freed only after `CancelMibChangeNotify2` returned (no callback in flight).
    let cb = unsafe { &*context.cast::<Callback>() };
    // Never unwind across the FFI boundary.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb()));
}

/// Calls a closure whenever a unicast address is added, removed or changed. Dropping the watcher
/// unregisters it and waits for any running callback to finish.
pub struct AddressChangeWatcher {
    handle: HANDLE,
    context: *mut Callback,
}

// SAFETY: the handle is only used to cancel the registration; the context is a Send + Sync closure.
unsafe impl Send for AddressChangeWatcher {}
// SAFETY: as above; no interior access through &self.
unsafe impl Sync for AddressChangeWatcher {}

impl std::fmt::Debug for AddressChangeWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddressChangeWatcher")
            .finish_non_exhaustive()
    }
}

impl AddressChangeWatcher {
    /// Registers `callback` for IPv4 and IPv6 address changes. The callback runs on an OS
    /// thread-pool thread and must be quick (send a signal, do not re-plan inline).
    pub fn new(callback: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        let context: *mut Callback = Box::into_raw(Box::new(Arc::new(callback) as Callback));
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: `on_change` matches the callback type; `context` stays valid until Drop.
        let rc: WIN32_ERROR = unsafe {
            NotifyUnicastIpAddressChange(
                AF_UNSPEC,
                Some(on_change),
                context.cast_const().cast(),
                false,
                &mut handle,
            )
        };
        if rc != ERROR_SUCCESS {
            // SAFETY: registration failed, so no callback can use `context`.
            drop(unsafe { Box::from_raw(context) });
            return Err(io::Error::from_raw_os_error(rc as i32));
        }
        Ok(Self { handle, context })
    }
}

impl Drop for AddressChangeWatcher {
    fn drop(&mut self) {
        // SAFETY: `handle` came from NotifyUnicastIpAddressChange; the call blocks until running
        // callbacks return, after which `context` can be freed.
        unsafe {
            CancelMibChangeNotify2(self.handle);
            drop(Box::from_raw(self.context));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_loopback_capable_machine() {
        let ifaces = list_interfaces().expect("GetAdaptersAddresses");
        assert!(!ifaces.is_empty());
        let all: Vec<IpAddr> = ifaces.iter().flat_map(|i| i.addresses.clone()).collect();
        assert!(all.iter().any(IpAddr::is_loopback), "{all:?}");
    }

    #[test]
    fn watcher_registers_and_unregisters() {
        let w = AddressChangeWatcher::new(|| {}).expect("register");
        drop(w);
    }
}
