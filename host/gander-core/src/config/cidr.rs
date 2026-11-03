// SPDX-License-Identifier: Apache-2.0
//! A small IP network type (`100.64.0.0/10`) for the generic trust adapter.
//!
//! Wildcard networks are unrepresentable: the unspecified address (`0.0.0.0`, `::`) and a zero
//! prefix length are rejected at parse time, so a config file can never ask the listener to cover
//! everything (security non-negotiable "never listen on 0.0.0.0 / ::").

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An IP network in CIDR notation. Host bits must be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IpCidr {
    addr: IpAddr,
    prefix: u8,
}

/// Why a CIDR string was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CidrError {
    /// Not `address/prefix`.
    #[error("expected address/prefix, for example 100.64.0.0/10")]
    Syntax,
    /// The prefix is larger than the address width.
    #[error("prefix length out of range")]
    PrefixRange,
    /// Bits are set after the prefix.
    #[error("host bits are set after the prefix")]
    HostBits,
    /// `0.0.0.0`, `::` or a `/0` network: it would cover every address.
    #[error("wildcard networks are not allowed")]
    Wildcard,
}

impl IpCidr {
    /// Builds a network, applying the same rules as parsing.
    pub fn new(addr: IpAddr, prefix: u8) -> Result<Self, CidrError> {
        let width = match addr {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        if prefix > width {
            return Err(CidrError::PrefixRange);
        }
        if prefix == 0 || addr.is_unspecified() {
            return Err(CidrError::Wildcard);
        }
        let cidr = Self { addr, prefix };
        if cidr.network_bits() != Self::bits(addr) {
            return Err(CidrError::HostBits);
        }
        Ok(cidr)
    }

    /// Network address.
    pub fn addr(&self) -> IpAddr {
        self.addr
    }

    /// Prefix length.
    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// Whether `ip` lies inside this network (same family only).
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_)) => {
                Self::bits(ip) & self.mask() == self.network_bits()
            }
            _ => false,
        }
    }

    fn bits(ip: IpAddr) -> u128 {
        match ip {
            IpAddr::V4(v4) => u128::from(u32::from(v4)),
            IpAddr::V6(v6) => u128::from(v6),
        }
    }

    fn mask(&self) -> u128 {
        let width: u32 = if self.addr.is_ipv4() { 32 } else { 128 };
        let host_bits = width - u32::from(self.prefix);
        let all: u128 = if width == 32 {
            u128::from(u32::MAX)
        } else {
            u128::MAX
        };
        if host_bits >= 128 {
            0
        } else {
            all & !((1u128 << host_bits) - 1)
        }
    }

    fn network_bits(&self) -> u128 {
        Self::bits(self.addr) & self.mask()
    }
}

impl FromStr for IpCidr {
    type Err = CidrError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (addr, prefix) = s.split_once('/').ok_or(CidrError::Syntax)?;
        let addr: IpAddr = addr.parse().map_err(|_| CidrError::Syntax)?;
        let prefix: u8 = prefix.parse().map_err(|_| CidrError::Syntax)?;
        Self::new(addr, prefix)
    }
}

impl fmt::Display for IpCidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

impl Serialize for IpCidr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for IpCidr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_private_ranges() {
        let tailnet: IpCidr = "100.64.0.0/10".parse().unwrap();
        assert!(tailnet.contains("100.101.102.103".parse().unwrap()));
        assert!(!tailnet.contains("100.128.0.1".parse().unwrap()));
        assert!(!tailnet.contains("fd7a:115c:a1e0::1".parse().unwrap()));
        let v6: IpCidr = "fd7a:115c:a1e0::/48".parse().unwrap();
        assert!(v6.contains("fd7a:115c:a1e0::1".parse().unwrap()));
        assert_eq!(v6.to_string(), "fd7a:115c:a1e0::/48");
        let host: IpCidr = "10.0.0.5/32".parse().unwrap();
        assert!(host.contains("10.0.0.5".parse().unwrap()));
    }

    #[test]
    fn wildcards_are_unrepresentable() {
        for s in ["0.0.0.0/0", "0.0.0.0/8", "::/0", "::/64", "10.0.0.0/0"] {
            assert_eq!(s.parse::<IpCidr>(), Err(CidrError::Wildcard), "{s}");
        }
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!("10.0.0.1/8".parse::<IpCidr>(), Err(CidrError::HostBits));
        assert_eq!("10.0.0.0/33".parse::<IpCidr>(), Err(CidrError::PrefixRange));
        assert_eq!("10.0.0.0".parse::<IpCidr>(), Err(CidrError::Syntax));
        assert_eq!("x/8".parse::<IpCidr>(), Err(CidrError::Syntax));
    }
}
