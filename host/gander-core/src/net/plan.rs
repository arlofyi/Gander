// SPDX-License-Identifier: Apache-2.0
//! The listener plan: which addresses the host binds, decided by the trust adapter.
//!
//! Input is a snapshot of the machine's interfaces ([`InterfaceInfo`], filled by the platform
//! crate, `gander_win::net::interfaces` on Windows) and the `[listener]` section of `config.toml`.
//! Output is a [`ListenerPlan`]: loopback plus the private-network addresses to serve.
//!
//! Invariant (security non-negotiable 1): a plan never contains an unspecified (wildcard) address
//! such as `0.0.0.0` or `::`. [`plan_listener`] filters them, and [`ListenerPlan::new`] refuses
//! them, so no code path can construct a plan that holds one.
//!
//! Adapter rules (pairing-and-trust spec, "Listener and network gate"):
//!
//! - **tailscale**: an interface is the Tailscale adapter when its GUID is
//!   [`TAILSCALE_ADAPTER_GUID`] or its description starts with [`TAILSCALE_ADAPTER_DESCRIPTION`].
//!   Only its addresses inside `100.64.0.0/10` or `fd7a:115c:a1e0::/48` are served.
//! - **generic**: the owner names an interface (alias or description) and/or private CIDRs. With an
//!   interface only, that interface's addresses are served; with CIDRs only, every local address
//!   inside them; with both, the interface's addresses inside the CIDRs. With neither, nothing but
//!   loopback is served (deny by default).
//! - `auto` picks tailscale when a Tailscale adapter with an address in range is present and up,
//!   otherwise generic.
//!
//! Addresses that are never served on the network side: unspecified, loopback (served separately),
//! multicast, broadcast, IPv4 link-local `169.254/16` and IPv6 link-local `fe80::/10` (they need a
//! scope id and are not private-network addresses).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::config::{AdapterChoice, IpCidr, ListenerConfig};
use crate::protocol::host::TrustAdapterKind;

/// GUID of the Wintun adapter Tailscale creates on Windows.
pub const TAILSCALE_ADAPTER_GUID: &str = "{37217669-42DA-4657-A55B-0D995D328250}";

/// Description prefix of the Tailscale adapter on Windows (Windows appends ` #2` to duplicates).
pub const TAILSCALE_ADAPTER_DESCRIPTION: &str = "Tailscale Tunnel";

/// Tailscale's IPv4 range (CGNAT `100.64.0.0/10`).
pub fn tailscale_v4_range() -> IpCidr {
    IpCidr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0)), 10).expect("valid constant network")
}

/// Tailscale's IPv6 range (`fd7a:115c:a1e0::/48`).
pub fn tailscale_v6_range() -> IpCidr {
    IpCidr::new(
        IpAddr::V6(Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 0)),
        48,
    )
    .expect("valid constant network")
}

/// Whether `ip` is inside a Tailscale range.
pub fn is_tailscale_address(ip: IpAddr) -> bool {
    tailscale_v4_range().contains(ip) || tailscale_v6_range().contains(ip)
}

/// One network interface as the platform reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InterfaceInfo {
    /// Adapter GUID in registry form (`{...}`), if the platform has one.
    pub guid: Option<String>,
    /// Friendly name / alias, for example `Ethernet` or `Tailscale`.
    pub alias: String,
    /// Description, for example `Tailscale Tunnel`.
    pub description: String,
    /// Operationally up.
    pub up: bool,
    /// Unicast addresses.
    pub addresses: Vec<IpAddr>,
}

impl InterfaceInfo {
    /// Whether this is the Tailscale adapter (by GUID or description).
    pub fn is_tailscale(&self) -> bool {
        let by_guid = self
            .guid
            .as_deref()
            .is_some_and(|g| g.eq_ignore_ascii_case(TAILSCALE_ADAPTER_GUID));
        let by_description = self
            .description
            .get(..TAILSCALE_ADAPTER_DESCRIPTION.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(TAILSCALE_ADAPTER_DESCRIPTION));
        by_guid || by_description
    }

    fn matches_name(&self, name: &str) -> bool {
        let name = name.trim();
        !name.is_empty()
            && (self.alias.eq_ignore_ascii_case(name)
                || self.description.eq_ignore_ascii_case(name)
                || self
                    .guid
                    .as_deref()
                    .is_some_and(|g| g.eq_ignore_ascii_case(name)))
    }
}

/// Error from [`ListenerPlan::new`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PlanError {
    /// The address is a wildcard (`0.0.0.0` or `::`).
    #[error("wildcard address {0} refused: the listener never binds a wildcard")]
    Wildcard(IpAddr),
}

/// The addresses the listener binds, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerPlan {
    adapter: TrustAdapterKind,
    port: u16,
    loopback: Vec<IpAddr>,
    network: Vec<IpAddr>,
}

impl ListenerPlan {
    /// Builds a plan. Refuses any unspecified address; sorts and deduplicates both lists.
    pub fn new(
        adapter: TrustAdapterKind,
        port: u16,
        loopback: Vec<IpAddr>,
        network: Vec<IpAddr>,
    ) -> Result<Self, PlanError> {
        if let Some(bad) = loopback
            .iter()
            .chain(&network)
            .find(|ip| ip.is_unspecified())
        {
            return Err(PlanError::Wildcard(*bad));
        }
        let mut loopback = loopback;
        let mut network = network;
        loopback.sort();
        loopback.dedup();
        network.sort();
        network.dedup();
        network.retain(|ip| !loopback.contains(ip));
        Ok(Self {
            adapter,
            port,
            loopback,
            network,
        })
    }

    /// The trust adapter that chose the network addresses.
    pub fn adapter(&self) -> TrustAdapterKind {
        self.adapter
    }

    /// The port every socket uses.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Loopback addresses (always served).
    pub fn loopback(&self) -> &[IpAddr] {
        &self.loopback
    }

    /// Private-network addresses chosen by the adapter (may be empty).
    pub fn network(&self) -> &[IpAddr] {
        &self.network
    }

    /// Every socket address to bind: loopback first, then network addresses.
    pub fn socket_addrs(&self) -> Vec<SocketAddr> {
        self.loopback
            .iter()
            .chain(&self.network)
            .map(|ip| SocketAddr::new(*ip, self.port))
            .collect()
    }

    /// Same plan with another port (tests use port 0).
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

/// Whether an address may ever be served on the network side.
pub fn is_servable_network_address(ip: IpAddr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => !(v4.is_link_local() || v4.is_broadcast()),
        // fe80::/10 link-local.
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

/// Default loopback addresses: `127.0.0.1` and `::1`.
pub fn default_loopback() -> Vec<IpAddr> {
    vec![
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ]
}

/// Tailscale addresses of the machine: the up Tailscale adapter's addresses in range.
pub fn tailscale_addresses(interfaces: &[InterfaceInfo]) -> Vec<IpAddr> {
    interfaces
        .iter()
        .filter(|i| i.up && i.is_tailscale())
        .flat_map(|i| i.addresses.iter().copied())
        .filter(|ip| is_tailscale_address(*ip) && is_servable_network_address(*ip))
        .collect()
}

fn generic_addresses(config: &ListenerConfig, interfaces: &[InterfaceInfo]) -> Vec<IpAddr> {
    let generic = &config.generic;
    let name = generic
        .interface
        .as_deref()
        .filter(|n| !n.trim().is_empty());
    if name.is_none() && generic.cidrs.is_empty() {
        return Vec::new();
    }
    interfaces
        .iter()
        .filter(|i| i.up)
        .filter(|i| name.is_none_or(|n| i.matches_name(n)))
        .flat_map(|i| i.addresses.iter().copied())
        .filter(|ip| generic.cidrs.is_empty() || generic.cidrs.iter().any(|c| c.contains(*ip)))
        .filter(|ip| is_servable_network_address(*ip))
        .collect()
}

/// Decides the listener plan for `config` on a machine with `interfaces`, serving `loopback`.
///
/// Never fails and never yields a wildcard: unspecified addresses are filtered before
/// [`ListenerPlan::new`] sees them.
pub fn plan_listener(
    config: &ListenerConfig,
    interfaces: &[InterfaceInfo],
    loopback: &[IpAddr],
) -> ListenerPlan {
    let tailscale = tailscale_addresses(interfaces);
    let (adapter, network) = match config.adapter {
        AdapterChoice::Tailscale => (TrustAdapterKind::Tailscale, tailscale),
        AdapterChoice::Generic => (
            TrustAdapterKind::Generic,
            generic_addresses(config, interfaces),
        ),
        AdapterChoice::Auto if !tailscale.is_empty() => (TrustAdapterKind::Tailscale, tailscale),
        AdapterChoice::Auto => (
            TrustAdapterKind::Generic,
            generic_addresses(config, interfaces),
        ),
    };
    let loopback: Vec<IpAddr> = loopback
        .iter()
        .copied()
        .filter(|ip| ip.is_loopback())
        .collect();
    ListenerPlan::new(adapter, config.port, loopback, network)
        .expect("plan inputs are filtered for wildcards")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GenericAdapterConfig;
    use proptest::prelude::*;

    fn ts_iface(addrs: &[&str]) -> InterfaceInfo {
        InterfaceInfo {
            guid: Some("{37217669-42da-4657-a55b-0d995d328250}".into()),
            alias: "Tailscale".into(),
            description: "Tailscale Tunnel".into(),
            up: true,
            addresses: addrs.iter().map(|a| a.parse().unwrap()).collect(),
        }
    }

    fn eth(addrs: &[&str]) -> InterfaceInfo {
        InterfaceInfo {
            guid: Some("{00000000-1111-2222-3333-444444444444}".into()),
            alias: "Ethernet".into(),
            description: "Intel(R) Ethernet".into(),
            up: true,
            addresses: addrs.iter().map(|a| a.parse().unwrap()).collect(),
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn auto_picks_tailscale_and_only_its_ranges() {
        let ifaces = [
            ts_iface(&[
                "100.101.102.103",
                "fd7a:115c:a1e0::1",
                "fe80::1",
                "192.168.9.9",
            ]),
            eth(&["192.168.1.10"]),
        ];
        let plan = plan_listener(&ListenerConfig::default(), &ifaces, &default_loopback());
        assert_eq!(plan.adapter(), TrustAdapterKind::Tailscale);
        assert_eq!(
            plan.network(),
            &[ip("100.101.102.103"), ip("fd7a:115c:a1e0::1")]
        );
        assert_eq!(plan.loopback(), &[ip("127.0.0.1"), ip("::1")]);
        assert_eq!(plan.socket_addrs().len(), 4);
    }

    #[test]
    fn tailscale_detected_by_description_only() {
        let mut i = ts_iface(&["100.64.0.1"]);
        i.guid = None;
        i.description = "Tailscale Tunnel #2".into();
        assert!(i.is_tailscale());
        assert_eq!(tailscale_addresses(&[i]), vec![ip("100.64.0.1")]);
    }

    #[test]
    fn tailscale_down_falls_back_to_generic_which_is_empty_by_default() {
        let mut i = ts_iface(&["100.64.0.1"]);
        i.up = false;
        let plan = plan_listener(&ListenerConfig::default(), &[i, eth(&["10.0.0.2"])], &[]);
        assert_eq!(plan.adapter(), TrustAdapterKind::Generic);
        assert!(plan.network().is_empty());
    }

    #[test]
    fn forced_tailscale_without_adapter_serves_loopback_only() {
        let cfg = ListenerConfig {
            adapter: AdapterChoice::Tailscale,
            ..ListenerConfig::default()
        };
        let plan = plan_listener(&cfg, &[eth(&["100.64.0.5"])], &default_loopback());
        assert!(plan.network().is_empty(), "range alone is not the adapter");
    }

    #[test]
    fn generic_by_interface_and_cidr() {
        let zt = InterfaceInfo {
            guid: None,
            alias: "ZeroTier One [8bd5124fd6]".into(),
            description: "ZeroTier Virtual Port".into(),
            up: true,
            addresses: vec![ip("10.147.17.5"), ip("fe80::5")],
        };
        let mut cfg = ListenerConfig {
            adapter: AdapterChoice::Generic,
            generic: GenericAdapterConfig {
                interface: Some("zerotier one [8bd5124fd6]".into()),
                cidrs: vec![],
            },
            ..ListenerConfig::default()
        };
        let ifaces = [zt.clone(), eth(&["192.168.1.10"])];
        let plan = plan_listener(&cfg, &ifaces, &[]);
        assert_eq!(plan.network(), &[ip("10.147.17.5")]);

        cfg.generic.interface = None;
        cfg.generic.cidrs = vec!["192.168.1.0/24".parse().unwrap()];
        let plan = plan_listener(&cfg, &ifaces, &[]);
        assert_eq!(plan.network(), &[ip("192.168.1.10")]);

        cfg.generic.interface = Some("Ethernet".into());
        cfg.generic.cidrs = vec!["10.0.0.0/8".parse().unwrap()];
        let plan = plan_listener(&cfg, &ifaces, &[]);
        assert!(plan.network().is_empty());
    }

    #[test]
    fn plan_refuses_wildcards_directly() {
        let err = ListenerPlan::new(TrustAdapterKind::Generic, 1, vec![], vec![ip("0.0.0.0")]);
        assert_eq!(err, Err(PlanError::Wildcard(ip("0.0.0.0"))));
        assert!(ListenerPlan::new(TrustAdapterKind::Generic, 1, vec![ip("::")], vec![]).is_err());
    }

    #[test]
    fn non_loopback_in_loopback_list_is_dropped() {
        let plan = plan_listener(
            &ListenerConfig::default(),
            &[],
            &[ip("10.0.0.1"), ip("127.0.0.1")],
        );
        assert_eq!(plan.loopback(), &[ip("127.0.0.1")]);
    }

    // ---- property test: the listener plan never contains a wildcard address ----

    fn arb_ip() -> impl Strategy<Value = IpAddr> {
        prop_oneof![
            // Wildcards and specials are deliberately over-represented.
            Just(IpAddr::V4(Ipv4Addr::UNSPECIFIED)),
            Just(IpAddr::V6(Ipv6Addr::UNSPECIFIED)),
            Just(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            Just(IpAddr::V6(Ipv6Addr::LOCALHOST)),
            Just(IpAddr::V4(Ipv4Addr::BROADCAST)),
            any::<u32>().prop_map(|b| IpAddr::V4(Ipv4Addr::from(b))),
            any::<u32>().prop_map(|b| IpAddr::V4(Ipv4Addr::from(0x6440_0000 | (b & 0x003f_ffff)))),
            any::<u128>().prop_map(|b| IpAddr::V6(Ipv6Addr::from(b))),
            any::<u64>().prop_map(|b| IpAddr::V6(Ipv6Addr::from(
                (0xfd7a_115c_a1e0_u128 << 80) | u128::from(b)
            ))),
        ]
    }

    fn arb_iface() -> impl Strategy<Value = InterfaceInfo> {
        (
            prop_oneof![
                Just(None),
                Just(Some(TAILSCALE_ADAPTER_GUID.to_owned())),
                "[{][0-9A-F-]{8,36}[}]".prop_map(Some),
            ],
            prop_oneof![Just("Tailscale".to_owned()), "[A-Za-z ]{0,12}"],
            prop_oneof![
                Just("Tailscale Tunnel".to_owned()),
                Just("ZeroTier Virtual Port".to_owned()),
                "[A-Za-z ]{0,16}"
            ],
            any::<bool>(),
            proptest::collection::vec(arb_ip(), 0..6),
        )
            .prop_map(|(guid, alias, description, up, addresses)| InterfaceInfo {
                guid,
                alias,
                description,
                up,
                addresses,
            })
    }

    fn arb_config() -> impl Strategy<Value = ListenerConfig> {
        (
            any::<u16>(),
            prop_oneof![
                Just(AdapterChoice::Auto),
                Just(AdapterChoice::Tailscale),
                Just(AdapterChoice::Generic)
            ],
            prop_oneof![
                Just(None),
                Just(Some("Ethernet".to_owned())),
                Just(Some("Tailscale".to_owned())),
                Just(Some(String::new()))
            ],
            proptest::collection::vec((arb_ip(), 1u8..=128), 0..3),
        )
            .prop_map(|(port, adapter, interface, raw_cidrs)| {
                // Only valid (non-wildcard) networks are representable in config; mask host bits.
                let cidrs = raw_cidrs
                    .into_iter()
                    .filter_map(|(ip, prefix)| {
                        let (bits, width) = match ip {
                            IpAddr::V4(v) => (u128::from(u32::from(v)) << 96, 32u8),
                            IpAddr::V6(v) => (u128::from(v), 128u8),
                        };
                        let prefix = prefix.min(width);
                        let mask = if prefix == 0 {
                            0
                        } else {
                            u128::MAX << (128 - u32::from(prefix))
                        };
                        let net = bits & mask;
                        let addr = match ip {
                            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::from((net >> 96) as u32)),
                            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::from(net)),
                        };
                        IpCidr::new(addr, prefix).ok()
                    })
                    .collect();
                ListenerConfig {
                    port,
                    adapter,
                    generic: GenericAdapterConfig { interface, cidrs },
                }
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2048))]

        #[test]
        fn listener_plan_never_contains_a_wildcard_address(
            config in arb_config(),
            interfaces in proptest::collection::vec(arb_iface(), 0..6),
            loopback in proptest::collection::vec(arb_ip(), 0..4),
        ) {
            let plan = plan_listener(&config, &interfaces, &loopback);
            for sa in plan.socket_addrs() {
                prop_assert!(!sa.ip().is_unspecified(), "wildcard {} in plan", sa);
            }
            for ip in plan.network() {
                prop_assert!(is_servable_network_address(*ip));
            }
            for ip in plan.loopback() {
                prop_assert!(ip.is_loopback());
            }
            if plan.adapter() == TrustAdapterKind::Tailscale {
                for ip in plan.network() {
                    prop_assert!(is_tailscale_address(*ip));
                }
            }
        }
    }
}
