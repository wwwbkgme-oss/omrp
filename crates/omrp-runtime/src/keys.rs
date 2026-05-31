//! API key management for the OMRP proxy server.
//!
//! When at least one key is registered, every `/v1/chat/completions` and
//! `/v1/models` request must carry a valid `Authorization: Bearer <key>` header.
//! The playground UI (`/` and `/playground`) and management endpoints
//! (`/v1/keys`) are always accessible without a key.
//!
//! Keys are stored in plain text at `~/.config/omrp/keys.json` — the file is
//! user-private and the format is intentionally simple.
//!
//! Key format:  `omrp-sk-<64 lowercase hex chars>`
//! Key ID:      `omrp_<8 lowercase hex chars>`

use std::io::Read;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ─── Types ────────────────────────────────────────────────────────────────────

/// One registered API key entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// Short opaque identifier, e.g. `omrp_3a7f1c2b`.
    pub id: String,
    /// Full bearer token, e.g. `omrp-sk-<64 hex>`.
    /// Stored and compared in plain text (local-only tool).
    pub key: String,
    /// Human-readable label set by the user, e.g. `"Cursor"`.
    pub label: String,
    /// Unix timestamp (seconds) when the key was created.
    pub created: u64,
}

// ─── KeyStore ─────────────────────────────────────────────────────────────────

/// In-memory key store backed by a JSON file on disk.
pub struct KeyStore {
    path: PathBuf,
    pub keys: Vec<ApiKey>,
}

impl KeyStore {
    /// Default path: `~/.config/omrp/keys.json`.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("omrp")
            .join("keys.json")
    }

    /// Load from disk, returning an empty store if the file doesn't exist.
    pub fn load(path: PathBuf) -> Self {
        let keys: Vec<ApiKey> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self { path, keys }
    }

    /// Persist the current key list to disk.
    pub fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create key dir: {e}"))?;
        }
        let json = serde_json::to_string_pretty(&self.keys)
            .map_err(|e| format!("JSON encode error: {e}"))?;
        std::fs::write(&self.path, json)
            .map_err(|e| format!("Cannot write keys file: {e}"))
    }

    /// Return `true` if the given bearer token is valid.
    ///
    /// When no keys are registered, every token is accepted (open access).
    pub fn validate(&self, bearer: &str) -> bool {
        if self.keys.is_empty() {
            return true;
        }
        self.keys.iter().any(|k| k.key == bearer)
    }

    /// Create and persist a new key.  Returns the full key (shown once).
    pub fn create(&mut self, label: &str) -> Result<ApiKey, String> {
        let entry = ApiKey {
            id:      generate_id(),
            key:     generate_key(),
            label:   label.to_string(),
            created: now_secs(),
        };
        self.keys.push(entry.clone());
        self.save()?;
        Ok(entry)
    }

    /// Delete a key by its `id` field.  Returns `true` if a key was removed.
    pub fn delete(&mut self, id: &str) -> Result<bool, String> {
        let before = self.keys.len();
        self.keys.retain(|k| k.id != id);
        let removed = self.keys.len() < before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// Returns `true` when at least one key is registered (auth required).
    pub fn auth_required(&self) -> bool {
        !self.keys.is_empty()
    }
}

// ─── Key / ID generation ─────────────────────────────────────────────────────

/// Generate a new API key: `omrp-sk-<64 hex chars>`.
fn generate_key() -> String {
    format!("omrp-sk-{}", random_hex(32))
}

/// Generate a short key ID: `omrp_<8 hex chars>`.
fn generate_id() -> String {
    format!("omrp_{}", random_hex(4))
}

/// Read `n` random bytes from `/dev/urandom` and hex-encode them.
/// Falls back to a time/pid-based hash if `/dev/urandom` is unavailable.
fn random_hex(n: usize) -> String {
    let mut buf = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            return hex_encode(&buf);
        }
    }
    // Fallback: mix time + process-id bytes
    let t = now_nanos();
    let pid = std::process::id() as u64;
    for (i, b) in buf.iter_mut().enumerate() {
        *b = (t.wrapping_add(pid).wrapping_mul(6364136223846793005)
            .wrapping_add(i as u64) >> 33) as u8;
    }
    hex_encode(&buf)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_key_format() {
        let k = generate_key();
        assert!(k.starts_with("omrp-sk-"), "key must start with omrp-sk-: {k}");
        assert_eq!(k.len(), 8 + 64, "key must be 72 chars: got {}", k.len());
    }

    #[test]
    fn test_generate_id_format() {
        let id = generate_id();
        assert!(id.starts_with("omrp_"), "id must start with omrp_: {id}");
        assert_eq!(id.len(), 5 + 8, "id must be 13 chars: got {}", id.len());
    }

    #[test]
    fn test_validate_open_access_when_empty() {
        let store = KeyStore { path: "/tmp/omrp-test-keys.json".into(), keys: vec![] };
        assert!(store.validate("anything"), "empty store must allow all tokens");
        assert!(!store.auth_required());
    }

    #[test]
    fn test_validate_requires_key_when_populated() {
        let k = ApiKey {
            id: "omrp_test1234".into(),
            key: "omrp-sk-aabbccdd".into(),
            label: "Test".into(),
            created: 0,
        };
        let store = KeyStore { path: "/tmp/omrp-test-keys.json".into(), keys: vec![k] };
        assert!(store.validate("omrp-sk-aabbccdd"));
        assert!(!store.validate("bad-token"));
        assert!(store.auth_required());
    }
}
