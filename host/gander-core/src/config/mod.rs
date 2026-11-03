// SPDX-License-Identifier: Apache-2.0
//! The host's configuration files and how they are loaded and saved.
//!
//! Three TOML files live in the per-user config directory (`%APPDATA%\Gander` on Windows):
//!
//! | File | Type | Contents |
//! | --- | --- | --- |
//! | `config.toml` | [`HostConfig`] | listener, trust options, stream options, shares and grants |
//! | `devices.toml` | [`DevicesFile`] | paired device records with token hashes |
//! | `apps.toml` | [`AppCatalogue`] | the Stream app catalogue |
//!
//! Every file carries `version = 1`, rejects unknown keys (a typo such as `show_hiden` must not
//! silently fall back to a default) and is validated on load and before save.
//!
//! This crate makes no OS calls, so reading and writing go through [`SecureFileStore`]. The
//! platform crate implements it; on Windows (`gander_win::config_store`) every write creates the
//! file with an owner-only DACL and atomically replaces the old one.

mod apps;
mod cidr;
mod devices;
mod host;

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

pub use apps::{AppCatalogue, AppEntry, CloseOnEnd, LaunchSpec, WindowMatch};
pub use cidr::{CidrError, IpCidr};
pub use devices::{DeviceRecord, DevicesFile, RecordedIdentity, TokenHash, TokenHashError};
pub use host::{
    AdapterChoice, CacheConfig, FirewallConfig, GenericAdapterConfig, HostConfig,
    IdlePromptDefault, ListenerConfig, MAX_UDP_PORT_SPAN, MIN_CACHE_CAP_MIB, MIN_UDP_PORT,
    PortRangeError, ShareConfig, ShareGrant, StreamConfig, TrustConfig, UdpPortRange, UiConfig,
    UiGpu, ZipConfig,
};

/// The schema version every config file is written with.
pub const CONFIG_VERSION: u32 = 1;

/// Largest config file accepted on load. Real files are a few KiB.
pub const MAX_CONFIG_BYTES: usize = 4 * 1024 * 1024;

/// Errors from parsing, validating, loading or saving a config file.
///
/// Messages never quote file contents: `devices.toml` holds token hashes, and parse errors report
/// only a line number and the parser's description.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// Reading or writing failed.
    #[error("{file}: {source}")]
    Io {
        /// The file involved.
        file: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The file is not valid TOML or does not match the schema.
    #[error("{file}: line {line}: {message}")]
    Parse {
        /// File name.
        file: String,
        /// 1-based line of the error, 0 if unknown.
        line: usize,
        /// Parser description (no file contents).
        message: String,
    },
    /// Serialising to TOML failed.
    #[error("{file}: cannot serialise: {message}")]
    Serialize {
        /// File name.
        file: String,
        /// Serializer description.
        message: String,
    },
    /// The file was written by a newer (or unknown) schema version.
    #[error("{file}: unsupported version {found} (this build reads version {supported})")]
    UnsupportedVersion {
        /// File name.
        file: String,
        /// Version in the file.
        found: u32,
        /// Version this build understands.
        supported: u32,
    },
    /// The file is larger than [`MAX_CONFIG_BYTES`].
    #[error("{file}: larger than {MAX_CONFIG_BYTES} bytes")]
    TooLarge {
        /// File name.
        file: String,
    },
    /// The file parsed but breaks a rule (duplicate id, wildcard network, and so on).
    #[error("{file}: {message}")]
    Invalid {
        /// File name.
        file: String,
        /// What is wrong.
        message: String,
    },
}

impl ConfigError {
    fn invalid(file: &str, message: impl Into<String>) -> Self {
        ConfigError::Invalid {
            file: file.to_owned(),
            message: message.into(),
        }
    }
}

/// Storage for config files, implemented by the platform crate.
///
/// Implementations must create written files readable and writable by the owning user only
/// (Windows: protected DACL with a single ACE for the current user; POSIX: mode 0600) and must
/// replace the target atomically so a crash never leaves a half-written file.
pub trait SecureFileStore {
    /// Reads the whole file. `Ok(None)` if it does not exist.
    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>>;

    /// Writes `contents` to `path` with an owner-only ACL, atomically replacing any existing
    /// file. Creates the parent directory (also owner-only) if it is missing.
    fn write_owner_only(&self, path: &Path, contents: &[u8]) -> io::Result<()>;
}

/// Behaviour shared by the three config files.
pub trait ConfigFile: Serialize + DeserializeOwned + Default {
    /// File name inside the config directory.
    const FILE_NAME: &'static str;

    /// The `version` value of this instance.
    fn version(&self) -> u32;

    /// Checks the rules TOML cannot express. Called on parse and before save.
    fn validate(&self) -> Result<(), ConfigError>;

    /// Parses and validates TOML text.
    fn from_toml_str(text: &str) -> Result<Self, ConfigError> {
        let value: Self = toml::from_str(text).map_err(|e| ConfigError::Parse {
            file: Self::FILE_NAME.to_owned(),
            line: e
                .span()
                .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1)
                .unwrap_or(0),
            message: e.message().to_owned(),
        })?;
        if value.version() != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                file: Self::FILE_NAME.to_owned(),
                found: value.version(),
                supported: CONFIG_VERSION,
            });
        }
        value.validate()?;
        Ok(value)
    }

    /// Validates and renders TOML text.
    fn to_toml_string(&self) -> Result<String, ConfigError> {
        self.validate()?;
        toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize {
            file: Self::FILE_NAME.to_owned(),
            message: e.to_string(),
        })
    }

    /// The file's full path inside `dir`.
    fn path_in(dir: &Path) -> PathBuf {
        dir.join(Self::FILE_NAME)
    }

    /// Loads the file from `dir`; a missing file yields the default (deny-by-default) value.
    fn load(store: &dyn SecureFileStore, dir: &Path) -> Result<Self, ConfigError> {
        let path = Self::path_in(dir);
        let bytes = store
            .read(&path)
            .map_err(|source| ConfigError::Io { file: path, source })?;
        let Some(bytes) = bytes else {
            return Ok(Self::default());
        };
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::TooLarge {
                file: Self::FILE_NAME.to_owned(),
            });
        }
        // A UTF-8 BOM written by a Windows editor is tolerated.
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        let text = std::str::from_utf8(bytes).map_err(|_| ConfigError::Parse {
            file: Self::FILE_NAME.to_owned(),
            line: 0,
            message: "not valid UTF-8".to_owned(),
        })?;
        Self::from_toml_str(text)
    }

    /// Validates and saves the file into `dir` with an owner-only ACL.
    fn save(&self, store: &dyn SecureFileStore, dir: &Path) -> Result<(), ConfigError> {
        let text = self.to_toml_string()?;
        let path = Self::path_in(dir);
        store
            .write_owner_only(&path, text.as_bytes())
            .map_err(|source| ConfigError::Io { file: path, source })
    }
}

fn default_version() -> u32 {
    CONFIG_VERSION
}

fn find_duplicate<'a, T: Ord + ?Sized>(items: impl Iterator<Item = &'a T>) -> Option<&'a T> {
    let mut seen = std::collections::BTreeSet::new();
    items.into_iter().find(|item| !seen.insert(*item))
}

#[cfg(test)]
pub(crate) mod memory_store {
    //! In-memory [`SecureFileStore`] for tests.
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Debug, Default)]
    pub(crate) struct MemoryStore {
        pub(crate) files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    }

    impl SecureFileStore for MemoryStore {
        fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
            Ok(self.files.borrow().get(path).cloned())
        }

        fn write_owner_only(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
            self.files
                .borrow_mut()
                .insert(path.to_owned(), contents.to_vec());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory_store::MemoryStore;
    use super::*;

    #[test]
    fn missing_files_load_as_defaults() {
        let store = MemoryStore::default();
        let dir = Path::new("cfg");
        assert_eq!(
            HostConfig::load(&store, dir).unwrap(),
            HostConfig::default()
        );
        assert_eq!(
            DevicesFile::load(&store, dir).unwrap(),
            DevicesFile::default()
        );
        assert_eq!(
            AppCatalogue::load(&store, dir).unwrap(),
            AppCatalogue::default()
        );
    }

    #[test]
    fn newer_version_is_refused() {
        let err = HostConfig::from_toml_str("version = 2\n").unwrap_err();
        assert!(matches!(
            err,
            ConfigError::UnsupportedVersion { found: 2, .. }
        ));
    }

    #[test]
    fn unknown_keys_are_refused_with_a_line_number() {
        let err = HostConfig::from_toml_str("version = 1\n\n[listener]\nprot = 1\n").unwrap_err();
        match err {
            ConfigError::Parse { line, message, .. } => {
                assert_eq!(line, 4);
                assert!(message.contains("prot"), "{message}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn bom_is_tolerated() {
        let store = MemoryStore::default();
        let dir = Path::new("cfg");
        store.files.borrow_mut().insert(
            HostConfig::path_in(dir),
            b"\xEF\xBB\xBFversion = 1\n".to_vec(),
        );
        assert_eq!(
            HostConfig::load(&store, dir).unwrap(),
            HostConfig::default()
        );
    }

    #[test]
    fn oversized_file_is_refused() {
        let store = MemoryStore::default();
        let dir = Path::new("cfg");
        store
            .files
            .borrow_mut()
            .insert(AppCatalogue::path_in(dir), vec![b' '; MAX_CONFIG_BYTES + 1]);
        assert!(matches!(
            AppCatalogue::load(&store, dir),
            Err(ConfigError::TooLarge { .. })
        ));
    }
}
