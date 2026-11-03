// SPDX-License-Identifier: Apache-2.0
//! Host and identity: `GET /v1/host`, `GET /v1/me`, `POST /v1/token/rotate`.

use std::net::IpAddr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::ids::{BearerToken, DeviceId, HostId, Timestamp};
use crate::clipboard::ClipboardMode;

/// `GET /v1/host` (no auth). Deliberately minimal: an unpaired peer learns almost nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostHello {
    /// Protocol versions the host speaks, for example `[1]`.
    pub protocol: Vec<u32>,
    /// Whether the owner has the "Add a device" window open.
    pub pairing_open: bool,
    /// The host id the client pins.
    pub host_id: HostId,
}

/// `GET /v1/me`: who the caller is, what the host is and what it can do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MeResponse {
    /// The calling device's record.
    pub device: DeviceSummary,
    /// The host.
    pub host: HostDetails,
    /// The private network as the host sees this caller.
    pub network: NetworkInfo,
    /// Advertised capabilities; additive changes appear here, never as a new protocol number.
    pub capabilities: Capabilities,
    /// Server-side limits.
    pub limits: Limits,
}

/// The calling device as the host knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSummary {
    /// Device id.
    pub id: DeviceId,
    /// Name given at pairing, for example "Pixel 9".
    pub name: String,
    /// When the device was paired.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: Timestamp,
    /// Last authenticated request, if any before this one.
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<Timestamp>,
}

/// The host machine. Also returned in the approved pairing response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostDetails {
    /// Host id (SPKI hash).
    pub id: HostId,
    /// Computer name.
    pub name: String,
    /// Operating system family.
    pub os: HostOs,
    /// OS version string, for example `10.0.26200`.
    pub os_version: String,
    /// Gander host version.
    pub app_version: String,
    /// Private-network addresses the host serves.
    pub addresses: Vec<IpAddr>,
    /// MagicDNS (or other private DNS) name, if known.
    pub magic_dns: Option<String>,
}

/// Host operating system family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostOs {
    /// Windows.
    Windows,
    /// macOS.
    Macos,
    /// Linux.
    Linux,
}

/// How the host sees the caller's network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInfo {
    /// The active trust adapter.
    pub adapter: TrustAdapterKind,
    /// The caller's private-network identity, when the adapter provides one.
    pub identity: Option<NetworkIdentity>,
    /// The network path to the caller; `null` when unknown or not applicable.
    pub relayed: Option<RelayState>,
}

/// Which trust adapter selects the listener addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustAdapterKind {
    /// Tailscale (WhoIs identity available).
    Tailscale,
    /// Any other private network; no identity.
    Generic,
}

/// Private-network identity of a peer (Tailscale WhoIs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkIdentity {
    /// Login name, treated as opaque (email on Tailscale, plain user name on Headscale).
    pub login: String,
    /// Node name, for example `pixel-9`.
    pub node: String,
}

/// Network path classification from `tailscale status`.
///
/// The protocol spec shows only `null`; the values come from the pairing spec's three path
/// classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayState {
    /// Direct peer-to-peer path.
    Direct,
    /// Through a Tailscale peer relay.
    PeerRelay,
    /// Through a DERP relay (a few Mbit/s; warn before streaming).
    Derp,
}

/// `capabilities` in `GET /v1/me`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Directory listing.
    pub list: bool,
    /// Name search.
    pub search: SearchCapability,
    /// `GET /recent`.
    pub recent: bool,
    /// `GET /events` WebSocket.
    pub events: bool,
    /// Thumbnail sizes the host renders.
    pub thumb_sizes: Vec<u32>,
    /// Largest preview width in pixels.
    pub preview_max_width: u32,
    /// Which preview kinds this host can produce.
    pub preview_kinds: PreviewKinds,
    /// Fetch features.
    pub fetch: FetchCapabilities,
    /// Stream features.
    pub stream: StreamCapabilities,
    /// Write access; always `false` in v1.
    pub write: bool,
    /// Why previews and thumbnails are off on this host, when they are (every `preview_kinds`
    /// is then `false`). Omitted when they work. Lets a client say "previews are unavailable on
    /// this computer" instead of a generic failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previews_unavailable: Option<PreviewsUnavailable>,
}

/// `capabilities.previews_unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewsUnavailable {
    /// `gander-render.exe` is not next to the host executable (for example a bare
    /// `cargo run -p gander-host`, which does not build it).
    RenderWorkerMissing,
    /// The thumbnail or preview cache could not be opened.
    CacheUnavailable,
}

impl PreviewsUnavailable {
    /// One sentence for the owner (tray, log).
    pub fn describe(self) -> &'static str {
        match self {
            PreviewsUnavailable::RenderWorkerMissing => {
                "Previews and thumbnails are off: gander-render.exe is missing next to gander-host.exe. Build both with `cargo xtask build-host` (or reinstall)."
            }
            PreviewsUnavailable::CacheUnavailable => {
                "Previews and thumbnails are off: the preview cache could not be opened (see the log)."
            }
        }
    }
}

/// Search support. On the wire: `false`, `true` (bounded substring walk) or `"indexed"`
/// (Windows Search index, a later capability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchCapability {
    /// No search.
    None,
    /// Bounded breadth-first substring match on names.
    Substring,
    /// Backed by the OS search index.
    Indexed,
}

impl Serialize for SearchCapability {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            SearchCapability::None => serializer.serialize_bool(false),
            SearchCapability::Substring => serializer.serialize_bool(true),
            SearchCapability::Indexed => serializer.serialize_str("indexed"),
        }
    }
}

impl<'de> Deserialize<'de> for SearchCapability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Bool(bool),
            Str(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Bool(false) => Ok(SearchCapability::None),
            Raw::Bool(true) => Ok(SearchCapability::Substring),
            Raw::Str(s) if s == "indexed" => Ok(SearchCapability::Indexed),
            // A newer host may add another search mode; it still searches.
            Raw::Str(_) => Ok(SearchCapability::Substring),
        }
    }
}

/// `capabilities.preview_kinds`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewKinds {
    /// Image previews.
    pub image: bool,
    /// PDF page renders.
    pub pdf: bool,
    /// Text previews.
    pub text: bool,
    /// Office document previews and their fidelity.
    pub office: OfficePreview,
    /// Video poster frames.
    pub video_poster: bool,
    /// Camera RAW decoding.
    pub raw: bool,
}

/// Best Office preview the host can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfficePreview {
    /// Embedded thumbnail only (page 1, low fidelity).
    Thumbnail,
    /// Full render (Office COM export or LibreOffice).
    Render,
    /// No Office previews.
    None,
}

/// `capabilities.fetch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchCapabilities {
    /// Single-range `Range` requests.
    pub range: bool,
    /// Hash algorithms `GET /hash` supports.
    pub hash: Vec<HashAlgo>,
    /// Folder download as a streamed zip (`GET /v1/zip`); absent on a host without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip: Option<ZipCapability>,
}

/// `capabilities.fetch.zip`: the limits of one folder download.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipCapability {
    /// Largest total size of the files in one zip, in bytes.
    pub max_bytes: u64,
    /// Most entries (files and folders) in one zip.
    pub max_entries: u64,
}

/// Content hash algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HashAlgo {
    /// BLAKE3.
    Blake3,
    /// SHA-256 (for clients without BLAKE3).
    Sha256,
}

/// `capabilities.stream`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamCapabilities {
    /// Stream mode is available on this host.
    pub enabled: bool,
    /// Video codecs the host can encode.
    pub codecs: Vec<VideoCodec>,
    /// Concurrent sessions per host (1 in v1).
    pub max_sessions: u32,
    /// Audio track available.
    pub audio: bool,
    /// Input injection available.
    pub input: bool,
    /// Host can resize the window to the phone viewport.
    pub fit_to_phone: bool,
    /// Which way clipboard text may cross during a stream (WP-56, the owner's `[stream]
    /// clipboard` setting; `to_phone` means the phone may read the computer's clipboard,
    /// `from_phone` that it may write it). Omitted when `off`; a client shows its two clipboard
    /// actions only for the directions that are on.
    #[serde(default, skip_serializing_if = "ClipboardMode::is_off")]
    pub clipboard: ClipboardMode,
}

/// Video codec for Stream mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    /// H.264 baseline or main.
    H264,
    /// HEVC / H.265.
    Hevc,
}

/// `limits` in `GET /v1/me`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    /// Maximum `limit` for list.
    pub list_max: u32,
    /// Maximum `limit` for search.
    pub search_max: u32,
    /// Per-device in-flight request limit.
    pub concurrency: u32,
}

/// `POST /v1/token/rotate` response. The old token stops working immediately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRotateResponse {
    /// The new bearer token.
    pub token: BearerToken,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_capability_wire_forms() {
        for (cap, json) in [
            (SearchCapability::None, "false"),
            (SearchCapability::Substring, "true"),
            (SearchCapability::Indexed, "\"indexed\""),
        ] {
            assert_eq!(serde_json::to_string(&cap).unwrap(), json);
            assert_eq!(serde_json::from_str::<SearchCapability>(json).unwrap(), cap);
        }
    }
}
