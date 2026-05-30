use std::path::PathBuf;
use sha2::{Sha256, Digest};
use serde::{Serialize, Deserialize};
use omrp_events::event::Event;

// ─── Checksum helpers ────────────────────────────────────────────────────────

/// Serialize a `[u8; 32]` as a lowercase hex string for human-readable ledger files.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let raw = String::deserialize(d)?;
        if raw.len() != 64 {
            return Err(serde::de::Error::custom("expected 64-char hex string"));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in raw.as_bytes().chunks(2).enumerate() {
            let hi = from_hex_nibble(chunk[0]).map_err(serde::de::Error::custom)?;
            let lo = from_hex_nibble(chunk[1]).map_err(serde::de::Error::custom)?;
            out[i] = (hi << 4) | lo;
        }
        Ok(out)
    }

    fn hex(bytes: &[u8; 32]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn from_hex_nibble(c: u8) -> Result<u8, &'static str> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err("invalid hex character"),
        }
    }
}

// ─── LedgerEntry ─────────────────────────────────────────────────────────────

/// A single tamper-evident entry in the append-only ledger.
///
/// Each entry's `checksum` is a SHA-256 hash of:
///   previous_checksum ‖ seq (LE u64) ‖ logical_time (LE u64) ‖ JSON-serialised event
///
/// The genesis entry uses `[0u8; 32]` as the previous checksum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LedgerEntry {
    pub seq: u64,
    pub logical_time: u64,
    pub event: Event,
    #[serde(with = "hex_bytes")]
    pub checksum: [u8; 32],
}

impl LedgerEntry {
    /// Construct and compute the checksum for a new entry.
    pub fn new(seq: u64, logical_time: u64, event: Event, previous_checksum: &[u8; 32]) -> Self {
        let checksum = compute_checksum(seq, logical_time, &event, previous_checksum);
        Self { seq, logical_time, event, checksum }
    }

    /// Verify this entry against its predecessor.
    pub fn verify_against(&self, previous: &LedgerEntry) -> bool {
        let expected = compute_checksum(
            self.seq,
            self.logical_time,
            &self.event,
            &previous.checksum,
        );
        self.checksum == expected
    }

    /// Verify the integrity of an entire chain.
    ///
    /// The genesis previous-checksum is `[0u8; 32]`.
    pub fn verify_chain(entries: &[LedgerEntry]) -> bool {
        let mut prev: [u8; 32] = [0u8; 32];
        for entry in entries {
            let expected = compute_checksum(entry.seq, entry.logical_time, &entry.event, &prev);
            if entry.checksum != expected {
                return false;
            }
            prev = entry.checksum;
        }
        true
    }
}

fn compute_checksum(
    seq: u64,
    logical_time: u64,
    event: &Event,
    previous_checksum: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(previous_checksum);
    hasher.update(seq.to_le_bytes());
    hasher.update(logical_time.to_le_bytes());
    // Serialise the event deterministically. serde_json produces stable output for
    // structs (field order matches declaration order).
    hasher.update(serde_json::to_vec(event).expect("event must be serialisable"));
    hasher.finalize().into()
}

// ─── LedgerError ─────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum LedgerError {
    Io(std::io::Error),
    Json(serde_json::Error),
    /// The SHA-256 chain is broken — the ledger has been tampered with.
    ChainIntegrityViolation,
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::Io(e) => write!(f, "IO error: {e}"),
            LedgerError::Json(e) => write!(f, "JSON serialization error: {e}"),
            LedgerError::ChainIntegrityViolation => {
                write!(f, "ledger chain integrity violation: file may have been tampered with")
            }
        }
    }
}

impl std::error::Error for LedgerError {}

impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self { LedgerError::Io(e) }
}

impl From<serde_json::Error> for LedgerError {
    fn from(e: serde_json::Error) -> Self { LedgerError::Json(e) }
}

// ─── LedgerStore ─────────────────────────────────────────────────────────────

/// Append-only, tamper-evident event store.
///
/// Phase 1 implementation: single JSON-Lines file, no rotation, synchronous IO.
///
/// Invariants:
/// - Entries are strictly monotonically increasing by `seq`.
/// - Each entry's `checksum` chains back to the previous one (SHA-256 over
///   `prev_checksum ‖ seq ‖ logical_time ‖ JSON(event)`).
/// - The file on disk is the source of truth; `persist()` rewrites it in full.
pub struct LedgerStore {
    path: PathBuf,
    entries: Vec<LedgerEntry>,
    last_checksum: [u8; 32],
}

impl LedgerStore {
    /// Create an empty in-memory ledger that will persist to `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            entries: Vec::new(),
            last_checksum: [0u8; 32],
        }
    }

    /// Append an event, compute its chained checksum, and return the entry.
    pub fn append(&mut self, event: Event) -> &LedgerEntry {
        let seq = self.entries.len() as u64 + 1;
        let logical_time = seq; // Phase 1: logical time equals seq.
        let entry = LedgerEntry::new(seq, logical_time, event, &self.last_checksum);
        self.last_checksum = entry.checksum;
        self.entries.push(entry);
        self.entries.last().unwrap()
    }

    /// All entries in insertion order.
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Number of entries in the ledger.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the ledger contains no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return all events in insertion order (for replay).
    pub fn replay(&self) -> Vec<Event> {
        self.entries.iter().map(|e| e.event.clone()).collect()
    }

    /// Verify the SHA-256 chain over all entries held in memory.
    pub fn verify(&self) -> bool {
        LedgerEntry::verify_chain(&self.entries)
    }

    /// Write the entire ledger to disk as JSON Lines (`path`).
    ///
    /// Each line is one `LedgerEntry` serialised as compact JSON.
    /// The file is (re)created on every call — suitable for Phase 1.
    pub fn persist(&self) -> Result<(), LedgerError> {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&self.path)?;
        for entry in &self.entries {
            let line = serde_json::to_string(entry)?;
            writeln!(file, "{line}")?;
        }
        Ok(())
    }

    /// Load a ledger from a JSON-Lines file and verify its chain integrity.
    ///
    /// Returns `Err(ChainIntegrityViolation)` if any checksum is invalid.
    pub fn load(path: PathBuf) -> Result<Self, LedgerError> {
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let mut entries: Vec<LedgerEntry> = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            entries.push(serde_json::from_str(line)?);
        }
        if !LedgerEntry::verify_chain(&entries) {
            return Err(LedgerError::ChainIntegrityViolation);
        }
        let last_checksum = entries.last().map(|e| e.checksum).unwrap_or([0u8; 32]);
        Ok(Self { path, entries, last_checksum })
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use omrp_events::event::{Event, ModelSource};
    use omrp_types::model::Model;

    fn make_model_added(id: &str) -> Event {
        Event::ModelAdded {
            model: Model::new(id, "test-provider"),
            source: ModelSource::Bundled,
        }
    }

    fn make_completion(id: &str) -> Event {
        Event::CompletionFinished {
            model_id: id.into(),
            latency_ms: 500,
            tokens_used: 100,
            success: true,
        }
    }

    // ── Checksum chain ─────────────────────────────────────────────────────

    #[test]
    fn test_first_entry_checksum_uses_zero_genesis() {
        let event = make_model_added("m1");
        let e = LedgerEntry::new(1, 1, event.clone(), &[0u8; 32]);
        let e2 = LedgerEntry::new(1, 1, event, &[0u8; 32]);
        assert_eq!(e.checksum, e2.checksum, "same inputs → same checksum");
    }

    #[test]
    fn test_chain_verify_passes_for_valid_chain() {
        let mut store = LedgerStore::new("/tmp/unused".into());
        store.append(make_model_added("m1"));
        store.append(make_completion("m1"));
        store.append(make_model_added("m2"));
        assert!(store.verify(), "valid chain must verify");
    }

    #[test]
    fn test_chain_verify_fails_for_tampered_entry() {
        let mut store = LedgerStore::new("/tmp/unused".into());
        store.append(make_model_added("m1"));
        store.append(make_completion("m1"));

        // Tamper: replace the event in the second entry without updating checksum.
        let mut entries = store.entries().to_vec();
        entries[1] = LedgerEntry {
            event: make_model_added("injected"),
            ..entries[1].clone()
        };
        assert!(
            !LedgerEntry::verify_chain(&entries),
            "tampered chain must not verify"
        );
    }

    #[test]
    fn test_checksum_differs_for_different_events() {
        let e1 = LedgerEntry::new(1, 1, make_model_added("m1"), &[0u8; 32]);
        let e2 = LedgerEntry::new(1, 1, make_model_added("different"), &[0u8; 32]);
        assert_ne!(e1.checksum, e2.checksum);
    }

    // ── Append / replay ────────────────────────────────────────────────────

    #[test]
    fn test_append_increments_seq() {
        let mut store = LedgerStore::new("/tmp/unused".into());
        let e1 = store.append(make_model_added("m1")).clone();
        let e2 = store.append(make_completion("m1")).clone();
        assert_eq!(e1.seq, 1);
        assert_eq!(e2.seq, 2);
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn test_replay_returns_events_in_order() {
        let mut store = LedgerStore::new("/tmp/unused".into());
        let ev1 = make_model_added("m1");
        let ev2 = make_completion("m1");
        store.append(ev1.clone());
        store.append(ev2.clone());
        let replayed = store.replay();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0], ev1);
        assert_eq!(replayed[1], ev2);
    }

    #[test]
    fn test_empty_ledger() {
        let store = LedgerStore::new("/tmp/unused".into());
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert!(store.verify());
        assert!(store.replay().is_empty());
    }

    // ── Persist / load round-trip ──────────────────────────────────────────

    #[test]
    fn test_persist_and_load_round_trip() {
        let path = std::path::PathBuf::from("/tmp/omrp_ledger_test_roundtrip.jsonl");
        let mut store = LedgerStore::new(path.clone());
        store.append(make_model_added("m1"));
        store.append(make_completion("m1"));
        store.append(make_model_added("m2"));
        store.persist().expect("persist must succeed");

        let loaded = LedgerStore::load(path.clone()).expect("load must succeed");
        assert_eq!(loaded.len(), 3);
        assert!(loaded.verify());
        assert_eq!(loaded.replay(), store.replay());

        // Cleanup.
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_load_missing_file_returns_empty_ledger() {
        let path = PathBuf::from("/tmp/omrp_nonexistent_12345.jsonl");
        let _ = std::fs::remove_file(&path); // ensure it doesn't exist
        let loaded = LedgerStore::load(path).expect("missing file → empty ledger");
        assert!(loaded.is_empty());
        assert!(loaded.verify());
    }

    #[test]
    fn test_load_detects_tampered_file() {
        let path = PathBuf::from("/tmp/omrp_ledger_tamper_test.jsonl");
        {
            let mut store = LedgerStore::new(path.clone());
            store.append(make_model_added("m1"));
            store.append(make_completion("m1"));
            store.persist().expect("persist must succeed");
        }

        // Read the raw file and corrupt the second entry's checksum field.
        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = content.lines().collect();
        // Replace the last character of the second line's checksum hex with 'x'.
        let corrupted = lines[1].replacen("\"checksum\":\"", "\"checksum\":\"X", 1);
        lines[1] = Box::leak(corrupted.into_boxed_str());
        std::fs::write(&path, lines.join("\n")).unwrap();

        let result = LedgerStore::load(path.clone());
        // Either JSON parse error (invalid hex 'X') or chain violation.
        assert!(result.is_err(), "tampered file must fail to load");

        let _ = std::fs::remove_file(path);
    }
}
