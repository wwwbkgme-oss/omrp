# OMRP Kernel Spec — Rust Core

> **Canonical reference** for Rust traits, reducer signatures, routing determinism rules, and ledger format.
> **Requires:** Rust 1.85+ (Edition 2024)
> **Architecture:** Event-sourced, deterministic, replay-safe

---

## 1. Core Traits

### 1.1 StateTransition

```rust
// omrp-core/src/transition.rs

/// Single mutation path: ALL state changes go through this.
/// No direct field mutation anywhere else in the system.
pub trait StateTransition<E> {
    fn apply(&self, state: &mut State, event: &E);
}

// Sugar: free function alias
pub type TransitionFn<E> = fn(&mut State, &E);
```

### 1.2 EventHandler

```rust
// omrp-core/src/handler.rs

/// Handles an event through the full pipeline.
pub trait EventHandler<E>: StateTransition<E> {
    /// Validate the event before any state change.
    fn validate(&self, event: &E) -> Result<(), ValidationError>;

    /// Route: determine if/how this event affects routing state.
    fn route(&self, state: &State, event: &E) -> RoutingDirective;
}

pub enum ValidationError {
    InvalidEvent(&'static str),
    DuplicateSequence(u64),
    OutOfOrderEvent { expected: u64, got: u64 },
}

pub enum RoutingDirective {
    /// No routing impact
    None,
    /// Re-evaluate routing state
    ReRoute,
    /// Trigger probe
    ProbeRequested { model_id: ModelId },
}
```

### 1.3 EventPipeline

```rust
// omrp-core/src/pipeline.rs

/// The canonical processing pipeline.
/// Pipeline: Validate → Route → Persist → Apply → Project
pub trait EventPipeline {
    type Event: Serialize + DeserializeOwned;
    type Error: std::error::Error;

    fn process(&mut self, event: Self::Event) -> Result<ProjectionView<State>, Self::Error>;

    /// Deterministic replay: same events → identical state.
    /// Used for recovery, debugging, and consistency checks.
    fn replay(&mut self, events: &[Self::Event]) -> State
    where
        Self::Event: Clone;
}

// ─── Projection ───
pub struct ProjectionView<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> ProjectionView<T> {
    pub fn new(state: T) -> Self {
        Self { inner: Arc::new(RwLock::new(state)) }
    }

    /// Read-only access. UI uses this exclusively.
    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.inner.read().expect("Poisoned lock"))
    }

    /// Internal: only pipeline calls this.
    pub fn write(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.inner.write().expect("Poisoned lock"));
    }
}
```

### 1.4 ProviderAdapter

```rust
// omrp-providers/src/adapter.rs

/// Provider-agnostic adapter interface.
/// Every provider (OpenRouter, Qwen, Kilo) implements this.
#[async_trait]
pub trait ProviderAdapter: Send + Sync + Debug {
    /// Unique provider identifier (e.g. "openrouter").
    fn provider_name(&self) -> &str;

    /// Send a completion request. Returns stream or response.
    async fn complete(
        &self,
        model: &Model,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError>;

    /// Check model health and reachability.
    async fn probe(&self, model: &Model) -> Result<ProbeResult, ProviderError>;

    /// List available models from this provider.
    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, ProviderError>;
}

#[derive(Debug)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub stream: bool,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug)]
pub struct CompletionResponse {
    pub content: String,
    pub model: String,
    pub usage: Option<TokenUsage>,
    pub latency_ms: u64,
}

#[derive(Debug)]
pub struct ProbeResult {
    pub reachable: bool,
    pub latency_ms: u64,
    pub model_available: bool,
}

#[derive(Debug)]
pub enum ProviderError {
    Network(String),
    Auth(String),
    RateLimited { retry_after: Option<u64> },
    ModelNotFound(String),
    Timeout(u64),
    Internal(String),
}
```

### 1.5 RouterEngine

```rust
// omrp-core/src/router.rs

/// The BKG-FMR routing engine (deterministic).
pub trait RouterEngine {
    /// Select the best model for a task.
    /// Deterministic: same state + same request → same result.
    fn select(&self, state: &State, request: &RouteRequest) -> RoutingDecision;

    /// Build fallback chain for a task type.
    fn fallback_chain(&self, state: &State, task_type: TaskType) -> Vec<ModelId>;
}

#[derive(Debug, Clone)]
pub struct RouteRequest {
    pub task_type: TaskType,
    pub max_latency_ms: Option<u64>,
    pub require_vision: bool,
    pub require_tool_use: bool,
    pub min_context_window: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub model_id: ModelId,
    pub score: f64,
    pub reason: RoutingReason,
    pub fallback_chain: Vec<ModelId>,
}

#[derive(Debug, Clone)]
pub enum RoutingReason {
    BestScore { score: f64 },
    Fallback { from: ModelId, cause: String },
    DegradeMode { reason: String },
    NoModelAvailable,
}
```

---

## 2. Events & Reducers

### 2.1 Event Enum

```rust
// omrp-events/src/event.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    // ─── Lifecycle ───
    DaemonStarted { version: String },
    DaemonStopped { reason: String },

    // ─── Model Discovery ───
    ModelAdded { model: Model, source: ModelSource },
    ModelRemoved { model_id: ModelId, reason: String },
    ConfigReloaded { source: String },

    // ─── Routing ───
    ModelSelected {
        model_id: ModelId,
        request: RouteRequest,
        score: f64,
        reason: RoutingReason,
    },
    FallbackTriggered {
        from: ModelId,
        to: ModelId,
        cause: String,
    },
    DegradeModeEnabled { model_id: ModelId, reason: String },

    // ─── Completion ───
    CompletionRequested {
        model_id: ModelId,
        task_type: TaskType,
        prompt_tokens: u32,
    },
    CompletionFinished {
        model_id: ModelId,
        latency_ms: u64,
        tokens_used: u64,
        success: bool,
    },
    ModelFailed {
        model_id: ModelId,
        error: ErrorKind,
    },

    // ─── Telemetry ───
    ProbeUpdated {
        model_id: ModelId,
        health: f32,
        latency_ms: u64,
    },
    ProbeFailed {
        model_id: ModelId,
        error: String,
    },
    ReportReceived {
        model_id: ModelId,
        success: bool,
        latency_ms: u64,
        tokens: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ErrorKind {
    RateLimited { retry_after: Option<u64> },
    Timeout { timeout_ms: u64 },
    AuthError,
    ModelNotAvailable,
    NetworkError(String),
    InternalError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskType {
    Code,
    Reasoning,
    Chat,
    Vision,
    Analysis,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelSource {
    Bundled,
    LocalConfig,
    UserContributed,
    AutoDiscovered,
}
```

### 2.2 Reducer Functions

```rust
// omrp-core/src/reducers.rs

/// ALL state transitions live here.
/// One reducer per Event variant.
/// Rules:
///   - Pure functions (no IO, no random, no SystemTime)
///   - Must be deterministic (same state + event → same result)
///   - Must handle idempotency (replay safety)

pub fn apply_model_added(state: &mut State, event: &Event) {
    let Event::ModelAdded { model, source } = event else { unreachable!() };
    // Don't re-add existing models (idempotent)
    if state.models.iter().any(|m| m.id == model.id) {
        return;
    }
    state.models.push(model.clone());
    state.health.insert(
        model.id.clone(),
        HealthStatus {
            score: 0.5,          // neutral start
            last_probe: SequencedInstant::EPOCH,
            last_success: SequencedInstant::EPOCH,
            success_rate: 0.0,
            avg_latency_ms: 0.0,
            stability_index: 0.5, // neutral start
            consecutive_failures: 0,
        },
    );
    state.ledger.push(event.clone());
}

pub fn apply_model_selected(state: &mut State, event: &Event) {
    let Event::ModelSelected { model_id, score, .. } = event else { unreachable!() };
    state.routing_cache.last_selected = Some(RoutingCacheEntry {
        model_id: model_id.clone(),
        score: *score,
        selected_at: state.current_time(),
    });
    state.ledger.push(event.clone());
}

pub fn apply_completion_finished(state: &mut State, event: &Event) {
    let Event::CompletionFinished { model_id, latency_ms, tokens_used, success } = event else { unreachable!() };
    if let Some(health) = state.health.get_mut(model_id) {
        if *success {
            health.success_rate = health.success_rate * 0.9 + 1.0 * 0.1; // EMA
            health.avg_latency_ms = health.avg_latency_ms * 0.9 + *latency_ms as f64 * 0.1;
            health.consecutive_failures = 0;
            health.last_success = state.current_time();
        } else {
            health.consecutive_failures += 1;
            health.success_rate = health.success_rate * 0.9 + 0.0 * 0.1;
        }
        health.stability_index = calculate_stability(health);
    }
    state.ledger.push(event.clone());
}

pub fn apply_model_failed(state: &mut State, event: &Event) {
    let Event::ModelFailed { model_id, error } = event else { unreachable!() };
    if let Some(health) = state.health.get_mut(model_id) {
        health.consecutive_failures += 1;
        health.score = f32::max(0.0, health.score - 0.2); // penalty
    }
    state.ledger.push(event.clone());
}

pub fn apply_probe_updated(state: &mut State, event: &Event) {
    let Event::ProbeUpdated { model_id, health: new_health, latency_ms } = event else { unreachable!() };
    if let Some(health) = state.health.get_mut(model_id) {
        health.score = *new_health;
        health.last_probe = state.current_time();
        health.avg_latency_ms = health.avg_latency_ms * 0.7 + *latency_ms as f64 * 0.3;
        if *new_health > 0.5 {
            health.last_success = state.current_time();
            health.consecutive_failures = 0;
        }
    }
    state.ledger.push(event.clone());
}

pub fn apply_fallback_triggered(state: &mut State, event: &Event) {
    let Event::FallbackTriggered { from, to, .. } = event else { unreachable!() };
    state.routing_cache.last_fallback = Some(FallbackEntry {
        from: from.clone(),
        to: to.clone(),
        at: state.current_time(),
    });
    state.ledger.push(event.clone());
}

// ─── Dispatcher ───

pub fn dispatch(state: &mut State, event: &Event) {
    match event {
        Event::ModelAdded { .. } => apply_model_added(state, event),
        Event::ModelRemoved { .. } => apply_model_removed(state, event),
        Event::ModelSelected { .. } => apply_model_selected(state, event),
        Event::CompletionFinished { .. } => apply_completion_finished(state, event),
        Event::ModelFailed { .. } => apply_model_failed(state, event),
        Event::ProbeUpdated { .. } => apply_probe_updated(state, event),
        Event::ProbeFailed { .. } => apply_probe_failed(state, event),
        Event::FallbackTriggered { .. } => apply_fallback_triggered(state, event),
        Event::DegradeModeEnabled { .. } => apply_degrade_mode_enabled(state, event),
        Event::ReportReceived { .. } => apply_report_received(state, event),
        Event::DaemonStarted { .. } => apply_daemon_started(state, event),
        Event::DaemonStopped { .. } => apply_daemon_stopped(state, event),
        Event::ConfigReloaded { .. } => apply_config_reloaded(state, event),
        Event::CompletionRequested { .. } => apply_completion_requested(state, event),
    }
}
```

---

## 3. State Model

```rust
// omrp-core/src/state.rs

#[derive(Debug, Clone)]
pub struct State {
    pub models: Vec<Model>,
    pub health: HashMap<ModelId, HealthStatus>,
    pub ledger: Vec<Event>,
    pub routing_cache: RoutingCache,
    pub config: Config,
    pub current_seq: u64,
}

impl State {
    pub fn new(config: Config) -> Self {
        Self {
            models: Vec::new(),
            health: HashMap::new(),
            ledger: Vec::new(),
            routing_cache: RoutingCache::default(),
            config,
            current_seq: 0,
        }
    }

    /// Current logical time (deterministic from ledger position)
    pub fn current_time(&self) -> SequencedInstant {
        SequencedInstant {
            seq: self.current_seq,
            logical_time: self.ledger.len() as u64,
        }
    }

    /// Advance sequence (called once per process())
    pub(crate) fn advance_seq(&mut self) {
        self.current_seq += 1;
    }
}

#[derive(Debug, Clone)]
pub struct HealthStatus {
    pub score: f32,              // 0.0 (dead) → 1.0 (perfect)
    pub last_probe: SequencedInstant,
    pub last_success: SequencedInstant,
    pub success_rate: f32,       // trailing EMA
    pub avg_latency_ms: f64,     // trailing EMA
    pub stability_index: f32,    // 0.0 (unstable) → 1.0 (rock solid)
    pub consecutive_failures: u32,
}

#[derive(Debug, Clone, Default)]
pub struct RoutingCache {
    pub last_selected: Option<RoutingCacheEntry>,
    pub last_fallback: Option<FallbackEntry>,
}

#[derive(Debug, Clone)]
pub struct RoutingCacheEntry {
    pub model_id: ModelId,
    pub score: f64,
    pub selected_at: SequencedInstant,
}

#[derive(Debug, Clone)]
pub struct FallbackEntry {
    pub from: ModelId,
    pub to: ModelId,
    pub at: SequencedInstant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SequencedInstant {
    pub seq: u64,
    pub logical_time: u64,
}

impl SequencedInstant {
    pub const EPOCH: Self = Self { seq: 0, logical_time: 0 };
}
```

---

## 4. BKG-FMR Scoring Engine

```rust
// omrp-core/src/scorer.rs

/// Best Known Garbage-Free Models Router scoring.
/// ALL deterministic. No randomness. No SystemTime.

pub struct Scorer {
    pub weights: ScoringWeights,
}

pub struct ScoringWeights {
    pub health: f64,       // default: 0.35
    pub latency: f64,      // default: 0.20
    pub success_rate: f64, // default: 0.25
    pub stability: f64,    // default: 0.20
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            health: 0.35,
            latency: 0.20,
            success_rate: 0.25,
            stability: 0.20,
        }
    }
}

impl Scorer {
    pub fn score(&self, model: &Model, health: &HealthStatus, task: &RouteRequest) -> f64 {
        let health_score = self.health_score(health);
        let latency_score = self.latency_score(health);
        let success_score = self.success_rate_score(health);
        let stability_score = self.stability_score(health);
        let capability_score = self.capability_match(model, task);

        // Weighted sum
        self.weights.health * health_score
            + self.weights.latency * latency_score
            + self.weights.success_rate * success_score
            + self.weights.stability * stability_score
            + capability_score // bonus for matching capabilities
    }

    fn health_score(&self, health: &HealthStatus) -> f64 {
        if health.consecutive_failures > 5 {
            return 0.0; // Garbage: too many failures
        }
        health.score as f64
    }

    fn latency_score(&self, health: &HealthStatus) -> f64 {
        if health.avg_latency_ms <= 0.0 {
            return 0.5; // unknown → neutral
        }
        // Lower is better. 500ms = 1.0, 10s = 0.0
        f64::max(0.0, 1.0 - (health.avg_latency_ms - 500.0) / 9500.0)
    }

    fn success_rate_score(&self, health: &HealthStatus) -> f64 {
        health.success_rate as f64
    }

    fn stability_score(&self, health: &HealthStatus) -> f64 {
        health.stability_index as f64
    }

    fn capability_match(&self, model: &Model, task: &RouteRequest) -> f64 {
        let mut score = 0.0;

        // Task type suitability
        if model.capabilities.task_suitability.contains(&task.task_type) {
            score += 0.15;
        }

        // Required capabilities
        if task.require_vision && model.capabilities.supports_vision {
            score += 0.10;
        }
        if task.require_tool_use && model.capabilities.supports_tool_use {
            score += 0.10;
        }
        if let Some(min_ctx) = task.min_context_window {
            if model.capabilities.context_window >= min_ctx {
                score += 0.05;
            }
        }

        score
    }

    /// Filter out garbage models before scoring.
    pub fn is_garbage(&self, health: &HealthStatus) -> bool {
        health.consecutive_failures > 10
            || health.score < 0.1
            || health.success_rate < 0.2 && health.last_success != SequencedInstant::EPOCH
    }
}
```

---

## 5. Routing Determinism Rules

```rust
// omrp-core/src/rules.rs

/// ─── ROUTING DETERMINISM RULES ───
///
/// These rules guarantee that the same ledger + same input → same routing decision.
///
/// Rule 1: NO external state in scoring
///   - `Scorer::score()` reads ONLY from State (models + health)
///   - No network calls, no file reads, no SystemTime
///
/// Rule 2: Deterministic tiebreaking
///   - If two models have the same score, break ties by:
///     1. Lower model_id (lexicographic)
///     2. Then by provider name
///   - Never by random, hash order, or undefined iteration
///
/// Rule 3: Fallback chain is computed once per selection
///   - `RouterEngine::fallback_chain()` is deterministic
///   - Order: score descending, then tiebreaker rule
///   - Garbage models (is_garbage()) are excluded
///
/// Rule 4: No mutable state during selection
///   - `select()` takes `&State`, not `&mut State`
///   - State mutations happen ONLY in reducers, after selection
///
/// Rule 5: Replay identity
///   - Same ledger file → same State after replay
///   - Same State + same RouteRequest → same RoutingDecision
///   - Enforced by integration tests

/// Deterministic tiebreaker for equal scores.
pub fn break_tie(a: &ModelId, b: &ModelId) -> Ordering {
    // Lexicographic comparison is deterministic across all platforms
    a.as_str().cmp(b.as_str())
}

/// Build a deterministic fallback chain.
pub fn fallback_chain(
    models: &[Model],
    health: &HashMap<ModelId, HealthStatus>,
    task: &RouteRequest,
    scorer: &Scorer,
    max_chain: usize,
) -> Vec<ModelId> {
    let mut scored: Vec<(f64, &Model)> = models
        .iter()
        .filter(|m| {
            let h = health.get(&m.id);
            h.map_or(true, |h| !scorer.is_garbage(h))
        })
        .map(|m| {
            let h = health.get(&m.id).cloned().unwrap_or(HealthStatus::default());
            (scorer.score(m, &h, task), m)
        })
        .collect();

    // Sort by score descending, then deterministic tiebreaker
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.partial_cmp(sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| break_tie(&a.id, &b.id))
    });

    scored.into_iter().take(max_chain).map(|(_, m)| m.id.clone()).collect()
}
```

---

## 6. Ledger Format

```rust
// omrp-events/src/ledger.rs

/// ─── LEDGER FORMAT ───
///
/// Format: JSON Lines (.jsonl)
/// One LedgerEntry per line.
/// Append-only. No deletes. No modifications.
///
/// File rotation: every 10,000 entries or 10MB, whichever comes first.
/// Files stored at: ~/.config/llm-free/ledger/{seq:08}.jsonl
///
/// Recovery: read all files in order → replay events → identical state.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Monotonic sequence number
    pub seq: u64,
    /// Logical time (deterministic, not wall clock)
    pub logical_time: u64,
    /// The event
    pub event: Event,
    /// SHA-256 of: previous_entry_hash + seq + logical_time + canonical_json(event)
    pub checksum: [u8; 32],
}

impl LedgerEntry {
    /// Create a new entry, computing checksum from previous.
    pub fn new(
        seq: u64,
        logical_time: u64,
        event: Event,
        previous_checksum: &[u8; 32],
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(previous_checksum);
        hasher.update(&seq.to_le_bytes());
        hasher.update(&logical_time.to_le_bytes());
        hasher.update(&serde_json::to_vec(&event).unwrap());
        
        Self {
            seq,
            logical_time,
            event,
            checksum: hasher.finalize().into(),
        }
    }

    /// Verify this entry against a previous entry.
    pub fn verify(&self, previous: &LedgerEntry) -> bool {
        let computed = Self::new(
            self.seq,
            self.logical_time,
            self.event.clone(),
            &previous.checksum,
        );
        self.checksum == computed.checksum
    }

    /// Verify the entire chain from genesis.
    pub fn verify_chain(entries: &[LedgerEntry]) -> bool {
        let mut prev_hash = [0u8; 32]; // genesis: zero hash
        for entry in entries {
            let expected = Self::new(
                entry.seq,
                entry.logical_time,
                entry.event.clone(),
                &prev_hash,
            );
            if entry.checksum != expected.checksum {
                return false;
            }
            prev_hash = entry.checksum;
        }
        true
    }
}

/// Ledger storage (append-only, file-per-segment)
pub struct LedgerStore {
    base_path: PathBuf,
    current_segment: u32,
    entries_since_rotation: u32,
    last_checksum: [u8; 32],
    writer: Option<BufWriter<File>>,
}

impl LedgerStore {
    pub fn new(base_path: PathBuf) -> Self {
        Self {
            base_path,
            current_segment: 0,
            entries_since_rotation: 0,
            last_checksum: [0u8; 32],
            writer: None,
        }
    }

    pub fn append(&mut self, event: Event, logical_time: u64) -> Result<LedgerEntry, LedgerError> {
        let seq = self.current_entry_count() + 1;
        let entry = LedgerEntry::new(seq, logical_time, event, &self.last_checksum);
        
        self.ensure_writer()?;
        let line = serde_json::to_string(&entry)?;
        writeln!(self.writer.as_mut().unwrap(), "{line}")?;
        
        self.last_checksum = entry.checksum;
        self.entries_since_rotation += 1;
        
        if self.entries_since_rotation >= 10_000 {
            self.rotate()?;
        }
        
        Ok(entry)
    }

    pub fn replay(&mut self) -> Result<Vec<Event>, LedgerError> {
        let mut events = Vec::new();
        let mut entries = Vec::new();
        
        // Read all segment files in order
        for segment_file in self.list_segments() {
            let file = File::open(&segment_file)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let entry: LedgerEntry = serde_json::from_str(&line?)?;
                entries.push(entry);
            }
        }

        // Verify chain integrity
        if !LedgerEntry::verify_chain(&entries) {
            return Err(LedgerError::ChainIntegrityViolation);
        }

        // Extract events in order
        for entry in &entries {
            events.push(entry.event.clone());
        }

        // Update state
        self.last_checksum = entries.last().map(|e| e.checksum).unwrap_or([0u8; 32]);
        
        Ok(events)
    }
}
```

---

## 7. Formal Invariants (Rust `debug_assert!` checks)

```rust
// omrp-core/src/invariants.rs

/// Runtime invariant checks (debug builds only).
pub fn assert_invariants(state: &State) {
    // Invariant 1: Every model in health map has a matching model entry
    for model_id in state.health.keys() {
        debug_assert!(
            state.models.iter().any(|m| &m.id == model_id),
            "Health entry without model: {model_id}"
        );
    }

    // Invariant 2: Ledger events are in sequence order
    for (i, event) in state.ledger.iter().enumerate() {
        if let Event::ModelSelected { .. } = event {
            // Last event must be a selection if routing_cache is set
            // (checked below)
        }
    }

    // Invariant 3: No garbage models in routing decisions
    if let Some(ref selected) = state.routing_cache.last_selected {
        if let Some(health) = state.health.get(&selected.model_id) {
            debug_assert!(
                health.consecutive_failures < 10,
                "Selected garbage model: {} ({} failures)",
                selected.model_id,
                health.consecutive_failures
            );
        }
    }

    // Invariant 4: Sequence numbers are monotonic
    #[cfg(debug_assertions)]
    {
        let mut last_seq = 0u64;
        for entry in &state.ledger {
            // Can't check seq without deserializing ledger
            // but we can check the dispatch order
        }
    }
}
```

---

## 8. Test Vectors (Determinism Verification)

```rust
// omrp-core/tests/determinism.rs

#[cfg(test)]
mod tests {
    use super::*;

    /// The same sequence of events MUST produce identical state.
    #[test]
    fn test_replay_identity() {
        let events = vec![
            Event::ModelAdded {
                model: model_fixture("openrouter/o4-mini"),
                source: ModelSource::Bundled,
            },
            Event::ProbeUpdated {
                model_id: "openrouter/o4-mini".into(),
                health: 0.9,
                latency_ms: 1200,
            },
            Event::CompletionFinished {
                model_id: "openrouter/o4-mini".into(),
                latency_ms: 1500,
                tokens_used: 250,
                success: true,
            },
        ];

        let state_a = replay_events(events.clone());
        let state_b = replay_events(events.clone());

        assert_eq!(
            serde_json::to_value(&state_a).unwrap(),
            serde_json::to_value(&state_b).unwrap(),
            "Replay must produce identical state"
        );
    }

    /// Same state + same request → same routing decision.
    #[test]
    fn test_deterministic_routing() {
        let state = test_state_with_models(3);
        let request = RouteRequest {
            task_type: TaskType::Code,
            max_latency_ms: None,
            require_vision: false,
            require_tool_use: false,
            min_context_window: None,
        };

        let engine = RouterEngineImpl::default();
        let decision_a = engine.select(&state, &request);
        let decision_b = engine.select(&state, &request);

        assert_eq!(decision_a.model_id, decision_b.model_id);
        assert_eq!(decision_a.score, decision_b.score);
        assert_eq!(decision_a.fallback_chain, decision_b.fallback_chain);
    }

    /// Tiebreaking is deterministic.
    #[test]
    fn test_deterministic_tiebreaking() {
        assert_eq!(break_tie("a", "b"), Ordering::Less);
        assert_eq!(break_tie("b", "a"), Ordering::Greater);
        assert_eq!(break_tie("a", "a"), Ordering::Equal);
    }

    /// Ledger chain verification catches tampering.
    #[test]
    fn test_ledger_integrity() {
        let mut entries = Vec::new();
        let mut prev_hash = [0u8; 32];

        for i in 0..5 {
            let event = Event::ProbeUpdated {
                model_id: format!("model-{i}"),
                health: 0.8,
                latency_ms: 1000,
            };
            let entry = LedgerEntry::new(i, i, event, &prev_hash);
            prev_hash = entry.checksum;
            entries.push(entry);
        }

        assert!(LedgerEntry::verify_chain(&entries));

        // Tamper with an event
        entries[2].event = Event::ProbeUpdated {
            model_id: "tampered".into(),
            health: 0.0,
            latency_ms: 999999,
        };

        assert!(!LedgerEntry::verify_chain(&entries));
    }
}
```

---

## 9. Dependencies (Phase 1)

```toml
# Cargo workspace

[workspace]
members = [
    "crates/omrp-core",
    "crates/omrp-events",
    "crates/omrp-runtime",
    "crates/omrp-providers",
    "crates/omrp-ui",
]

# omrp-core
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
thiserror = "2"

# omrp-events
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# omrp-runtime
[dependencies]
omrp-core = { path = "../omrp-core" }
omrp-events = { path = "../omrp-events" }
omrp-providers = { path = "../omrp-providers" }
axum = "0.8"
tokio = { version = "1", features = ["full"] }
tower-http = { version = "0.6", features = ["cors"] }
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# omrp-providers
[dependencies]
omrp-core = { path = "../omrp-core" }
omrp-events = { path = "../omrp-events" }
async-trait = "0.1"
reqwest = { version = "0.12", features = ["json", "stream"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# omrp-ui (Dioxus)
[dependencies]
omrp-core = { path = "../omrp-core" }
dioxus = { version = "0.6", features = ["full"] }
dioxus-logger = "0.6"
```

---

> **Nächster Schritt:** Implementation Plan — genaue Tasks für den Bau von Phase 1.
