// SPDX-License-Identifier: MIT OR Apache-2.0
//! Node-local credentials for. Paths are resolved against the server configuration
//! directory. Limits belong to node configuration because admission precedes database access.
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fmt,
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

/// Credential admission failure. No variant contains secret bytes or parser diagnostics.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CredentialError {
    /// Unknown fields or malformed table/value types.
    #[error(
        "invalid [ai] configuration: expected max_providers, max_key_bytes, max_request_bytes and provider.<name>.key_file only"
    )]
    Configuration,
    /// Node admission setting outside its safety bounds.
    #[error("{setting} must be in {range}")]
    Limit {
        /// Node configuration setting name.
        setting: &'static str,
        /// Inclusive accepted range.
        range: &'static str,
    },
    /// More configured providers than the node admits.
    #[error("ai.provider exceeds ai.max_providers")]
    ProviderCount,
    /// Name cannot be mapped safely to a declared provider record.
    #[error("ai.provider names must be 1..=63 ASCII letters, digits, underscores or hyphens")]
    ProviderName,
    /// File admission error; reason contains only fixed text and mode/owner metadata.
    #[error("AI provider '{provider}' key file '{path}': {reason}")]
    File {
        /// Provider configuration name.
        provider: String,
        /// Configured path (never file contents).
        path: PathBuf,
        /// Sanitized diagnostic.
        reason: String,
    },
    /// No credential is configured for the explicitly selected provider.
    #[error("erp.ai.provider_unconfigured: provider '{provider}'")]
    Unconfigured {
        /// Provider selected by the route; never a fallback name.
        provider: String,
    },
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    max_providers: Option<usize>,
    max_key_bytes: Option<usize>,
    max_request_bytes: Option<usize>,
    #[serde(default)]
    provider: BTreeMap<String, RawProvider>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvider {
    key_file: PathBuf,
}

/// Validated credentials. Secret buffers are erased on drop and omitted from Debug.
/// This type deliberately implements neither Clone nor serialization; adapters live in this addon.
#[non_exhaustive]
pub struct Credentials {
    keys: BTreeMap<String, Zeroizing<Vec<u8>>>,
    request_bytes: usize,
}
pub(crate) const DEFAULT_REQUEST_BYTES: usize = 1024 * 1024;
const MIN_REQUEST_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;

impl Default for Credentials {
    fn default() -> Self {
        Self {
            keys: BTreeMap::new(),
            request_bytes: DEFAULT_REQUEST_BYTES,
        }
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("providers", &self.keys.len())
            .finish_non_exhaustive()
    }
}
impl Credentials {
    /// Validate a complete `[ai]` table and atomically load its protected credential files.
    /// Missing `[ai]` means an empty registry. Reload by constructing a replacement, then swapping
    /// only after success; a partial failed reload never yields a usable registry.
    ///
    /// # Errors
    /// Refuses malformed configuration, invalid names/limits, missing or insecure files,
    /// wrong owners, symlinks, special files, empty/oversized keys and non-ASCII token contents.
    pub fn load(value: Option<&toml::Value>, directory: &Path) -> Result<Self, CredentialError> {
        let raw: RawConfig = match value {
            Some(value) => value
                .clone()
                .try_into()
                .map_err(|_| CredentialError::Configuration)?,
            None => RawConfig::default(),
        };
        // These defaults are operator-adjustable node admission limits, bounded below and above.
        let max_providers = raw.max_providers.unwrap_or(32);
        let max_key_bytes = raw.max_key_bytes.unwrap_or(16384);
        if !(1..=128).contains(&max_providers) {
            return Err(CredentialError::Limit {
                setting: "ai.max_providers",
                range: "1..=128",
            });
        }
        if !(1..=65536).contains(&max_key_bytes) {
            return Err(CredentialError::Limit {
                setting: "ai.max_key_bytes",
                range: "1..=65536",
            });
        }
        let request_bytes = raw.max_request_bytes.unwrap_or(DEFAULT_REQUEST_BYTES);
        if !(MIN_REQUEST_BYTES..=MAX_REQUEST_BYTES).contains(&request_bytes) {
            return Err(CredentialError::Limit {
                setting: "ai.max_request_bytes",
                range: "65536..=4194304",
            });
        }
        if raw.provider.len() > max_providers {
            return Err(CredentialError::ProviderCount);
        }
        let mut keys = BTreeMap::new();
        for (name, provider) in raw.provider {
            if name.is_empty()
                || name.len() > 63
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return Err(CredentialError::ProviderName);
            }
            let path = directory.join(provider.key_file);
            keys.insert(name.clone(), read_key(&name, &path, max_key_bytes)?);
        }
        Ok(Self {
            keys,
            request_bytes,
        })
    }

    pub(crate) fn request_bytes(&self) -> usize {
        self.request_bytes
    }

    /// Number of configured credentials; does not expose names, paths or key bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether this node has no configured AI credentials.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Require the explicitly selected credential, without trying another provider.
    /// # Errors
    /// `Unconfigured` names the selected provider when no key is configured.
    pub fn require(&self, provider: &str) -> Result<(), CredentialError> {
        if self.keys.contains_key(provider) {
            Ok(())
        } else {
            Err(CredentialError::Unconfigured {
                provider: provider.to_owned(),
            })
        }
    }
}
fn read_key(
    provider: &str,
    path: &Path,
    max: usize,
) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
    read_key_for_owner(provider, path, max, rustix::process::geteuid().as_raw())
}

// The service identity is supplied only by read_key in production. Keeping descriptor admission
// together permits testing an owner mismatch without changing accounts on shared build hosts.
fn read_key_for_owner(
    provider: &str,
    path: &Path,
    max: usize,
    service_uid: u32,
) -> Result<Zeroizing<Vec<u8>>, CredentialError> {
    let refused = |reason: String| CredentialError::File {
        provider: provider.to_owned(),
        path: path.to_owned(),
        reason,
    };
    // O_NONBLOCK prevents FIFO/device opens from hanging before the regular-file check.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| refused("cannot open protected regular file".into()))?;
    let metadata = file
        .metadata()
        .map_err(|_| refused("cannot inspect file".into()))?;
    if !metadata.is_file() {
        return Err(refused("must be a regular file".into()));
    }
    let mode = metadata.mode() & 0o7777;
    if mode != 0o600 {
        return Err(refused(format!("mode {mode:04o} must be 0600")));
    }
    if metadata.uid() != service_uid {
        return Err(refused("must be owned by the service user".into()));
    }
    let invalid = || refused("expected a nonempty ASCII token within max_key_bytes".into());
    if metadata.len() > max as u64 + 1 {
        return Err(invalid());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(max as u64 + 2)
        .read_to_end(&mut bytes)
        .map_err(|_| refused("cannot read file".into()))?;
    // A single terminal LF is accepted for ordinary operator-managed secret files.
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.is_empty() || bytes.len() > max || !bytes.iter().all(u8::is_ascii_graphic) {
        return Err(invalid());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    #[allow(clippy::unwrap_used, reason = "test assertions")]
    fn file_owner_is_checked_against_expected_service_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixture.key");
        std::fs::write(&path, "synthetic-owner-fixture").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let owner = std::fs::metadata(&path).unwrap().uid();
        let bytes = read_key_for_owner("fixture", &path, 64, owner).unwrap();
        assert_eq!(bytes.as_slice(), b"synthetic-owner-fixture");
        let error = read_key_for_owner("fixture", &path, 64, owner ^ 1).unwrap_err();
        assert!(matches!(error, CredentialError::File { .. }));
        assert_eq!(
            error.to_string(),
            format!(
                "AI provider 'fixture' key file '{}': must be owned by the service user",
                path.display()
            )
        );
        assert!(!format!("{error:?}").contains("synthetic-owner-fixture"));
    }
}
