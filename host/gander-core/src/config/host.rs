// SPDX-License-Identifier: Apache-2.0
//! `config.toml`: listener, trust and stream options, shares and per-device grants.
//!
//! Example:
//!
//! ```toml
//! version = 1
//!
//! [listener]
//! port = 42637
//! adapter = "auto"          # auto | tailscale | generic
//!
//! [listener.generic]        # used only by the generic adapter
//! interface = "ZeroTier One [8bd5124fd6]"
//! cidrs = ["10.147.17.0/24"]
//!
//! [trust]
//! allow_tagged_peers = false
//! strict_node_binding = false
//!
//! [stream]
//! idle_prompt_seconds = 120
//! idle_prompt_default = "allow"   # allow | deny
//! capture_border = true
//! clipboard = "off"               # off | to_phone | from_phone | both (WP-56, text only)
//! audio = true                    # send the streamed window's own sound (WP-55)
//!
//! [cache]
//! cap_mib = 2048            # thumbnails and previews together; read at start
//!
//! [zip]                     # folder download (GET /v1/zip); read on every request
//! max_mib = 4096            # largest total size of the files in one zip (4 GiB)
//! max_entries = 20000       # most files and folders in one zip
//!
//! [ui]
//! gpu = "integrated"        # integrated | discrete | auto: the GPU the host's own windows use
//!
//! [firewall]                # read by the installer scripts (installer/firewall-rules.ps1)
//! udp_ports = "50000-50099" # WebRTC host candidates: the Stream engine binds inside this range
//! allow_public_profile = false
//!
//! [[share]]
//! id = "shr_01JC0000000000000000000000"
//! name = "Projects"
//! root = 'D:\Projects'
//! show_hidden = false
//! show_path = false
//! hidden_patterns = ["**/.git"]
//! stream_default = "ask"
//!
//! [[share.grant]]
//! device = "dev_01JC0000000000000000000000"
//! permissions = ["list", "preview", "fetch"]
//! ```
//!
//! There is deliberately no bind-address setting: the listener plan comes from the trust adapter
//! and never contains a wildcard address.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{ConfigError, ConfigFile, IpCidr, default_version, find_duplicate};
use crate::clipboard::ClipboardMode;
use crate::protocol::files::{Permission, StreamDecision};
use crate::protocol::{DEFAULT_PORT, DeviceId, ShareId};

const FILE: &str = "config.toml";

/// Contents of `config.toml`. The default has no shares, so nothing is reachable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// Schema version, always 1.
    #[serde(default = "default_version")]
    pub version: u32,
    /// Listener options.
    #[serde(default)]
    pub listener: ListenerConfig,
    /// Pairing and token trust options.
    #[serde(default)]
    pub trust: TrustConfig,
    /// Stream mode options.
    #[serde(default)]
    pub stream: StreamConfig,
    /// Thumbnail and preview cache options.
    #[serde(default, skip_serializing_if = "CacheConfig::is_default")]
    pub cache: CacheConfig,
    /// Settings and tray window options.
    #[serde(default, skip_serializing_if = "UiConfig::is_default")]
    pub ui: UiConfig,
    /// Windows Firewall rule settings (the installer scripts read this section).
    #[serde(default, skip_serializing_if = "FirewallConfig::is_default")]
    pub firewall: FirewallConfig,
    /// Folder download (zip) limits.
    #[serde(default, skip_serializing_if = "ZipConfig::is_default")]
    pub zip: ZipConfig,
    /// Shares, in display order.
    #[serde(default, rename = "share", skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<ShareConfig>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            version: super::CONFIG_VERSION,
            listener: ListenerConfig::default(),
            trust: TrustConfig::default(),
            stream: StreamConfig::default(),
            cache: CacheConfig::default(),
            ui: UiConfig::default(),
            firewall: FirewallConfig::default(),
            zip: ZipConfig::default(),
            shares: Vec::new(),
        }
    }
}

/// `[listener]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ListenerConfig {
    /// TCP port (default 42637).
    pub port: u16,
    /// Which trust adapter selects the addresses.
    pub adapter: AdapterChoice,
    /// Settings of the generic adapter.
    pub generic: GenericAdapterConfig,
}

impl Default for ListenerConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            adapter: AdapterChoice::Auto,
            generic: GenericAdapterConfig::default(),
        }
    }
}

/// Trust adapter selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterChoice {
    /// Tailscale if its adapter is present, otherwise generic.
    #[default]
    Auto,
    /// Always Tailscale.
    Tailscale,
    /// Always the generic adapter.
    Generic,
}

/// `[listener.generic]`: the owner's choice of interface or private networks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GenericAdapterConfig {
    /// Interface name (alias) chosen in the tray UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    /// Private networks whose local addresses are served. Wildcards are unrepresentable.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cidrs: Vec<IpCidr>,
}

/// `[trust]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TrustConfig {
    /// Allow Tailscale tagged nodes to pair (default false).
    pub allow_tagged_peers: bool,
    /// Refuse requests whose Tailscale node id differs from pairing (default: warn only).
    pub strict_node_binding: bool,
}

/// `[stream]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StreamConfig {
    /// Last input more recent than this means someone is at the computer (default 120).
    pub idle_prompt_seconds: u32,
    /// What happens when the attended-host toast gets no answer in 10 s.
    pub idle_prompt_default: IdlePromptDefault,
    /// Keep the OS capture border on (default true).
    pub capture_border: bool,
    /// Which way text may cross between the phone and the computer's clipboard during a stream
    /// (default `off`; WP-56, text only, 64 KiB).
    pub clipboard: ClipboardMode,
    /// Send the streamed application's own sound with the picture (default true; WP-55). Only
    /// that application's process tree is captured, never the whole computer; off means no audio
    /// track at all. Read when the host starts.
    pub audio: bool,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            idle_prompt_seconds: 120,
            idle_prompt_default: IdlePromptDefault::Allow,
            capture_border: true,
            clipboard: ClipboardMode::Off,
            audio: true,
        }
    }
}

/// Outcome of an unanswered attended-host prompt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdlePromptDefault {
    /// Start streaming (it is the owner's own device).
    #[default]
    Allow,
    /// Do not stream.
    Deny,
}

/// `[ui]`: the host's own windows (settings, Add a device, toasts).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UiConfig {
    /// Which GPU draws the host's windows on a hybrid-GPU laptop (default `integrated`). Stream
    /// encoding is not affected: it always picks the encoder itself (NVIDIA first). Read when the
    /// host starts.
    pub gpu: UiGpu,
}

impl UiConfig {
    /// Whether this is the default (so the section is left out of a saved file).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `[ui] gpu`: the GPU preference for the host's windows on a hybrid-GPU (Optimus, PowerXpress)
/// laptop. On a machine with one GPU every value draws on that GPU.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiGpu {
    /// The integrated GPU: opening a window does not wake the discrete GPU (saves battery).
    #[default]
    Integrated,
    /// The discrete GPU (NVIDIA or AMD), for heavier future windows.
    Discrete,
    /// Discrete on mains power, integrated on battery (decided when a window session starts).
    Auto,
}

impl UiGpu {
    /// Whether the discrete GPU is asked for, given the power state.
    pub fn wants_discrete(self, on_battery: bool) -> bool {
        match self {
            UiGpu::Integrated => false,
            UiGpu::Discrete => true,
            UiGpu::Auto => !on_battery,
        }
    }
}

/// Smallest cache cap the settings accept (MiB).
pub const MIN_CACHE_CAP_MIB: u32 = 64;

/// `[cache]`: how much disk the thumbnail and preview caches may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CacheConfig {
    /// Total cap in MiB, split evenly between the thumbnail and the preview cache (default 2048,
    /// that is 1 GiB each). Read when the host starts.
    pub cap_mib: u32,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self { cap_mib: 2048 }
    }
}

impl CacheConfig {
    /// Whether this is the default (so the section is left out of a saved file).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The cap of each of the two caches, in bytes.
    pub fn per_cache_bytes(&self) -> u64 {
        u64::from(self.cap_mib) * 1024 * 1024 / 2
    }
}

/// Default folder-zip size limit (MiB): 4 GiB.
pub const DEFAULT_ZIP_MAX_MIB: u32 = 4096;

/// Default folder-zip entry limit (files and folders).
pub const DEFAULT_ZIP_MAX_ENTRIES: u32 = 20_000;

/// `[zip]`: the limits of `GET /v1/zip`, the folder download (WP-68). A folder over either limit
/// is refused with `413 too_large` before anything is streamed. Read on every request, so a
/// change applies at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ZipConfig {
    /// Largest total size of the files in one zip, in MiB (default 4096, that is 4 GiB).
    pub max_mib: u32,
    /// Most entries (files and folders below the requested folder) in one zip (default 20,000).
    pub max_entries: u32,
}

impl Default for ZipConfig {
    fn default() -> Self {
        Self {
            max_mib: DEFAULT_ZIP_MAX_MIB,
            max_entries: DEFAULT_ZIP_MAX_ENTRIES,
        }
    }
}

impl ZipConfig {
    /// Whether this is the default (so the section is left out of a saved file).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The size limit in bytes.
    pub fn max_bytes(&self) -> u64 {
        u64::from(self.max_mib) * 1024 * 1024
    }
}

/// Lowest UDP port the firewall range may start at (below it are the well-known and registered
/// service ports).
pub const MIN_UDP_PORT: u16 = 1024;

/// Most ports the firewall range may span. One WebRTC session needs a handful.
pub const MAX_UDP_PORT_SPAN: u16 = 1000;

/// An inclusive UDP port range, written `"50000-50099"` in `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UdpPortRange {
    start: u16,
    end: u16,
}

/// Why a UDP port range was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PortRangeError {
    /// Not `first-last`.
    #[error("expected first-last, for example 50000-50099")]
    Syntax,
    /// The first port is above the last.
    #[error("the first port is above the last")]
    Reversed,
    /// The range starts below [`MIN_UDP_PORT`].
    #[error("ports below {MIN_UDP_PORT} are not allowed")]
    TooLow,
    /// The range spans more than [`MAX_UDP_PORT_SPAN`] ports.
    #[error("the range may span at most {MAX_UDP_PORT_SPAN} ports")]
    TooWide,
}

impl UdpPortRange {
    /// The default WebRTC host-candidate range, 50000-50099.
    pub const DEFAULT: Self = Self {
        start: 50000,
        end: 50099,
    };

    /// Builds a range, applying the same rules as parsing.
    pub fn new(start: u16, end: u16) -> Result<Self, PortRangeError> {
        if start > end {
            return Err(PortRangeError::Reversed);
        }
        if start < MIN_UDP_PORT {
            return Err(PortRangeError::TooLow);
        }
        if end - start >= MAX_UDP_PORT_SPAN {
            return Err(PortRangeError::TooWide);
        }
        Ok(Self { start, end })
    }

    /// First port.
    pub fn start(&self) -> u16 {
        self.start
    }

    /// Last port (inclusive).
    pub fn end(&self) -> u16 {
        self.end
    }
}

impl Default for UdpPortRange {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl std::fmt::Display for UdpPortRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

impl std::str::FromStr for UdpPortRange {
    type Err = PortRangeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (a, b) = s.split_once('-').ok_or(PortRangeError::Syntax)?;
        let start = a.trim().parse().map_err(|_| PortRangeError::Syntax)?;
        let end = b.trim().parse().map_err(|_| PortRangeError::Syntax)?;
        Self::new(start, end)
    }
}

impl Serialize for UdpPortRange {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for UdpPortRange {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// `[firewall]`: what the installer's Windows Firewall rules open. The host itself never edits
/// firewall rules; `installer/firewall-rules.ps1` reads this section (and `listener.port` and
/// `listener.generic.cidrs`) when the owner runs the elevated network setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FirewallConfig {
    /// UDP ports for WebRTC host candidates (default 50000-50099). The Stream engine binds its
    /// media sockets inside this range, and the UDP allow rule opens exactly this range.
    pub udp_ports: UdpPortRange,
    /// Also open the rules on Public-profile networks (default false). Tailscale's adapter is
    /// sometimes classified Public; the rules stay limited to the private-network source
    /// addresses either way.
    pub allow_public_profile: bool,
}

impl Default for FirewallConfig {
    fn default() -> Self {
        Self {
            udp_ports: UdpPortRange::DEFAULT,
            allow_public_profile: false,
        }
    }
}

impl FirewallConfig {
    /// Whether this is the default (so the section is left out of a saved file).
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// `[[share]]`: one shared folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareConfig {
    /// Share id.
    pub id: ShareId,
    /// Display name.
    pub name: String,
    /// Absolute root path as chosen by the owner. The path resolver canonicalises it.
    pub root: PathBuf,
    /// List hidden and system entries (default false).
    #[serde(default)]
    pub show_hidden: bool,
    /// Send the root path to devices as `path_display` (default false).
    #[serde(default)]
    pub show_path: bool,
    /// Globs on the share-relative path that are hidden and unresolvable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_patterns: Vec<String>,
    /// Stream decision for unknown handlers in this share (default ask).
    #[serde(default = "default_stream_decision")]
    pub stream_default: StreamDecision,
    /// Per-device permissions. A device without a grant cannot see the share.
    #[serde(default, rename = "grant", skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<ShareGrant>,
}

fn default_stream_decision() -> StreamDecision {
    StreamDecision::Ask
}

/// `[[share.grant]]`: what one device may do in a share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareGrant {
    /// Device id.
    pub device: DeviceId,
    /// Granted permissions. `stream` is trusted-device access (ADR-0006).
    pub permissions: Vec<Permission>,
}

impl ShareConfig {
    /// Permissions of `device` in this share; empty (deny) when there is no grant.
    pub fn permissions_for(&self, device: &DeviceId) -> &[Permission] {
        self.grants
            .iter()
            .find(|g| &g.device == device)
            .map(|g| g.permissions.as_slice())
            .unwrap_or(&[])
    }

    /// Whether `device` holds `permission` here.
    pub fn allows(&self, device: &DeviceId, permission: Permission) -> bool {
        self.permissions_for(device).contains(&permission)
    }
}

impl HostConfig {
    /// Shares the device can see (it holds at least one permission).
    pub fn shares_for<'a>(
        &'a self,
        device: &'a DeviceId,
    ) -> impl Iterator<Item = &'a ShareConfig> + 'a {
        self.shares
            .iter()
            .filter(move |s| !s.permissions_for(device).is_empty())
    }

    /// Looks up a share by id.
    pub fn share(&self, id: &ShareId) -> Option<&ShareConfig> {
        self.shares.iter().find(|s| &s.id == id)
    }
}

impl ConfigFile for HostConfig {
    const FILE_NAME: &'static str = FILE;

    fn version(&self) -> u32 {
        self.version
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.listener.port == 0 {
            return Err(ConfigError::invalid(FILE, "listener.port must not be 0"));
        }
        if self.cache.cap_mib < MIN_CACHE_CAP_MIB {
            return Err(ConfigError::invalid(
                FILE,
                format!("cache.cap_mib must be at least {MIN_CACHE_CAP_MIB}"),
            ));
        }
        if self.zip.max_mib == 0 || self.zip.max_entries == 0 {
            return Err(ConfigError::invalid(
                FILE,
                "zip.max_mib and zip.max_entries must be at least 1",
            ));
        }
        if let Some(dup) = find_duplicate(self.shares.iter().map(|s| &s.id)) {
            return Err(ConfigError::invalid(
                FILE,
                format!("duplicate share id {dup}"),
            ));
        }
        for share in &self.shares {
            if share.id.as_str().is_empty() {
                return Err(ConfigError::invalid(FILE, "share id must not be empty"));
            }
            if share.name.trim().is_empty() {
                return Err(ConfigError::invalid(
                    FILE,
                    format!("share {} has an empty name", share.id),
                ));
            }
            if !share.root.is_absolute() {
                return Err(ConfigError::invalid(
                    FILE,
                    format!("share {} root must be an absolute path", share.id),
                ));
            }
            if share.hidden_patterns.iter().any(|p| p.trim().is_empty()) {
                return Err(ConfigError::invalid(
                    FILE,
                    format!("share {} has an empty hidden pattern", share.id),
                ));
            }
            if let Some(dup) = find_duplicate(share.grants.iter().map(|g| &g.device)) {
                return Err(ConfigError::invalid(
                    FILE,
                    format!("share {} grants device {dup} twice", share.id),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs_root() -> &'static str {
        if cfg!(windows) {
            "D:\\\\Projects"
        } else {
            "/srv/projects"
        }
    }

    fn sample() -> String {
        format!(
            r#"version = 1

[listener]
port = 42637
adapter = "generic"

[listener.generic]
interface = "ZeroTier One"
cidrs = ["10.147.17.0/24", "fd00:1::/64"]

[trust]
allow_tagged_peers = false
strict_node_binding = true

[stream]
idle_prompt_seconds = 90
idle_prompt_default = "deny"
capture_border = true

[[share]]
id = "shr_a"
name = "Projects"
root = "{}"
hidden_patterns = ["**/.git"]

[[share.grant]]
device = "dev_phone"
permissions = ["list", "preview", "fetch"]

[[share.grant]]
device = "dev_tablet"
permissions = ["list", "stream"]
"#,
            abs_root()
        )
    }

    #[test]
    fn parses_and_roundtrips() {
        let cfg = HostConfig::from_toml_str(&sample()).unwrap();
        assert_eq!(cfg.listener.adapter, AdapterChoice::Generic);
        assert_eq!(cfg.listener.generic.cidrs.len(), 2);
        assert!(cfg.trust.strict_node_binding);
        assert_eq!(cfg.stream.idle_prompt_default, IdlePromptDefault::Deny);
        let share = &cfg.shares[0];
        assert_eq!(share.stream_default, StreamDecision::Ask);
        assert!(!share.show_hidden);
        let back = HostConfig::from_toml_str(&cfg.to_toml_string().unwrap()).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn ui_gpu_defaults_to_integrated() {
        let cfg = HostConfig::from_toml_str(&sample()).unwrap();
        assert_eq!(cfg.ui.gpu, UiGpu::Integrated);
        // The default section is not written.
        assert!(
            !HostConfig::default()
                .to_toml_string()
                .unwrap()
                .contains("[ui]")
        );
        for (text, gpu) in [
            ("integrated", UiGpu::Integrated),
            ("discrete", UiGpu::Discrete),
            ("auto", UiGpu::Auto),
        ] {
            let toml = format!(
                "version = 1
[ui]
gpu = \"{text}\"
"
            );
            let cfg = HostConfig::from_toml_str(&toml).unwrap();
            assert_eq!(cfg.ui.gpu, gpu);
            let back = HostConfig::from_toml_str(&cfg.to_toml_string().unwrap()).unwrap();
            assert_eq!(back.ui.gpu, gpu);
        }
        assert!(!UiGpu::Integrated.wants_discrete(false));
        assert!(UiGpu::Discrete.wants_discrete(true));
        assert!(UiGpu::Auto.wants_discrete(false));
        assert!(!UiGpu::Auto.wants_discrete(true));
        let typo = "version = 1
[ui]
gpu = \"nvidia\"
";
        assert!(matches!(
            HostConfig::from_toml_str(typo),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn clipboard_is_off_unless_the_owner_turns_it_on() {
        // Absent: off (also in a config written before WP-56).
        let cfg = HostConfig::from_toml_str(&sample()).unwrap();
        assert_eq!(cfg.stream.clipboard, ClipboardMode::Off);
        assert_eq!(HostConfig::default().stream.clipboard, ClipboardMode::Off);
        for (text, mode) in [
            ("off", ClipboardMode::Off),
            ("to_phone", ClipboardMode::ToPhone),
            ("from_phone", ClipboardMode::FromPhone),
            ("both", ClipboardMode::Both),
        ] {
            let toml = format!("version = 1\n[stream]\nclipboard = \"{text}\"\n");
            let cfg = HostConfig::from_toml_str(&toml).unwrap();
            assert_eq!(cfg.stream.clipboard, mode);
            let back = HostConfig::from_toml_str(&cfg.to_toml_string().unwrap()).unwrap();
            assert_eq!(back.stream.clipboard, mode);
        }
        // A typo must not silently fall back to a default.
        let typo = "version = 1\n[stream]\nclipboard = \"on\"\n";
        assert!(matches!(
            HostConfig::from_toml_str(typo),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn deny_by_default() {
        let cfg = HostConfig::from_toml_str(&sample()).unwrap();
        let phone = DeviceId::from("dev_phone");
        let stranger = DeviceId::from("dev_stranger");
        let share = &cfg.shares[0];
        assert!(share.allows(&phone, Permission::Fetch));
        assert!(!share.allows(&phone, Permission::Stream));
        assert!(share.permissions_for(&stranger).is_empty());
        assert_eq!(cfg.shares_for(&phone).count(), 1);
        assert_eq!(cfg.shares_for(&stranger).count(), 0);
        assert!(HostConfig::default().shares.is_empty());
    }

    #[test]
    fn wildcard_cidr_is_refused() {
        let text = "version = 1\n[listener.generic]\ncidrs = [\"0.0.0.0/0\"]\n";
        assert!(matches!(
            HostConfig::from_toml_str(text),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn rule_violations_are_refused() {
        let dup_share = format!(
            "{}\n[[share]]\nid = \"shr_a\"\nname = \"Again\"\nroot = \"{}\"\n",
            sample(),
            abs_root()
        );
        assert!(matches!(
            HostConfig::from_toml_str(&dup_share),
            Err(ConfigError::Invalid { .. })
        ));
        let relative = "version = 1\n[[share]]\nid = \"s\"\nname = \"n\"\nroot = \"rel\"\n";
        assert!(matches!(
            HostConfig::from_toml_str(relative),
            Err(ConfigError::Invalid { .. })
        ));
        let port0 = "version = 1\n[listener]\nport = 0\n";
        assert!(matches!(
            HostConfig::from_toml_str(port0),
            Err(ConfigError::Invalid { .. })
        ));
        let dup_grant = format!(
            "{}\n[[share.grant]]\ndevice = \"dev_phone\"\npermissions = []\n",
            sample()
        );
        assert!(matches!(
            HostConfig::from_toml_str(&dup_grant),
            Err(ConfigError::Invalid { .. })
        ));
    }

    #[test]
    fn cache_section_defaults_roundtrips_and_has_a_floor() {
        let default = HostConfig::default();
        assert_eq!(default.cache.cap_mib, 2048);
        assert_eq!(default.cache.per_cache_bytes(), 1024 * 1024 * 1024);
        // The default is left out of a saved file.
        assert!(!default.to_toml_string().unwrap().contains("[cache]"));

        let mut cfg = HostConfig::default();
        cfg.cache.cap_mib = 512;
        let text = cfg.to_toml_string().unwrap();
        assert!(text.contains("cap_mib = 512"), "{text}");
        assert_eq!(HostConfig::from_toml_str(&text).unwrap(), cfg);

        let tiny = "version = 1\n[cache]\ncap_mib = 8\n";
        assert!(matches!(
            HostConfig::from_toml_str(tiny),
            Err(ConfigError::Invalid { .. })
        ));
        let typo = "version = 1\n[cache]\ncap = 512\n";
        assert!(matches!(
            HostConfig::from_toml_str(typo),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn zip_section_defaults_roundtrips_and_refuses_zero() {
        let default = HostConfig::default();
        assert_eq!(default.zip.max_mib, 4096);
        assert_eq!(default.zip.max_bytes(), 4 * 1024 * 1024 * 1024);
        assert_eq!(default.zip.max_entries, 20_000);
        // The default is left out of a saved file.
        assert!(!default.to_toml_string().unwrap().contains("[zip]"));

        let mut cfg = HostConfig::default();
        cfg.zip.max_mib = 512;
        cfg.zip.max_entries = 500;
        let text = cfg.to_toml_string().unwrap();
        assert!(
            text.contains("max_mib = 512") && text.contains("max_entries = 500"),
            "{text}"
        );
        assert_eq!(HostConfig::from_toml_str(&text).unwrap(), cfg);

        // A section with one key keeps the other default.
        let one = HostConfig::from_toml_str("version = 1\n[zip]\nmax_entries = 7\n").unwrap();
        assert_eq!((one.zip.max_mib, one.zip.max_entries), (4096, 7));
        for bad in [
            "version = 1\n[zip]\nmax_mib = 0\n",
            "version = 1\n[zip]\nmax_entries = 0\n",
        ] {
            assert!(matches!(
                HostConfig::from_toml_str(bad),
                Err(ConfigError::Invalid { .. })
            ));
        }
        assert!(matches!(
            HostConfig::from_toml_str("version = 1\n[zip]\nmax = 1\n"),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn firewall_section_defaults_roundtrips_and_validates_the_range() {
        let default = HostConfig::default();
        assert_eq!(default.firewall.udp_ports.to_string(), "50000-50099");
        assert!(!default.firewall.allow_public_profile);
        // The default is left out of a saved file.
        assert!(!default.to_toml_string().unwrap().contains("[firewall]"));

        let text =
            "version = 1\n[firewall]\nudp_ports = \"49200-49299\"\nallow_public_profile = true\n";
        let cfg = HostConfig::from_toml_str(text).unwrap();
        assert_eq!(
            cfg.firewall.udp_ports,
            UdpPortRange::new(49200, 49299).unwrap()
        );
        assert!(cfg.firewall.allow_public_profile);
        let saved = cfg.to_toml_string().unwrap();
        assert!(saved.contains("udp_ports = \"49200-49299\""), "{saved}");
        assert_eq!(HostConfig::from_toml_str(&saved).unwrap(), cfg);

        for bad in [
            "80-90",
            "50099-50000",
            "50000",
            "50000-60000",
            "a-b",
            "50000-70000",
            "",
        ] {
            let text = format!("version = 1\n[firewall]\nudp_ports = \"{bad}\"\n");
            assert!(
                matches!(
                    HostConfig::from_toml_str(&text),
                    Err(ConfigError::Parse { .. })
                ),
                "{bad:?} must be refused"
            );
        }
        let typo = "version = 1\n[firewall]\nudp_port = \"50000-50099\"\n";
        assert!(matches!(
            HostConfig::from_toml_str(typo),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn udp_port_ranges_parse_and_print() {
        assert_eq!("50000-50099".parse(), Ok(UdpPortRange::DEFAULT));
        assert_eq!(
            " 50000 - 50000 "
                .parse::<UdpPortRange>()
                .unwrap()
                .to_string(),
            "50000-50000"
        );
        assert_eq!(
            "1023-2000".parse::<UdpPortRange>(),
            Err(PortRangeError::TooLow)
        );
        assert_eq!(
            "2000-1999".parse::<UdpPortRange>(),
            Err(PortRangeError::Reversed)
        );
        assert_eq!(
            "2000-3000".parse::<UdpPortRange>(),
            Err(PortRangeError::TooWide)
        );
        assert_eq!("2000-2999".parse::<UdpPortRange>().unwrap().end(), 2999);
        assert_eq!("2000".parse::<UdpPortRange>(), Err(PortRangeError::Syntax));
        assert_eq!(
            "2000-x".parse::<UdpPortRange>(),
            Err(PortRangeError::Syntax)
        );
    }

    #[test]
    fn write_permission_does_not_exist() {
        let text = format!(
            "version = 1\n[[share]]\nid = \"s\"\nname = \"n\"\nroot = \"{}\"\n[[share.grant]]\ndevice = \"d\"\npermissions = [\"write\"]\n",
            abs_root()
        );
        assert!(HostConfig::from_toml_str(&text).is_err());
    }
}
