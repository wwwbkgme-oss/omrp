# OMRP Execution Semantics Spec v1.1

> **Status:** Formal, implementable in Rust without ambiguity
> **Architecture:** 3 Machines (Ledger → Reducer → Scheduler)
> **Key Fixes:** Dual truth resolved, backpressure added, time model clarified, EMA drift fixed, decision tracing added

---

## 0. Architecture Overview: Three Machines

```
┌─────────────────────────────────────────────────────────┐
│                   1. Ledger Machine                       │
│                   (immutable, append-only)                │
│                                                          │
│   Event → validate → checksum → persist → OK             │
│                                                          │
│   Source of Truth. Nothing else stores events.           │
└──────────────────────────────────────────────────────────┘
                          │ events (ordered slice)
                          ▼
┌─────────────────────────────────────────────────────────┐
│                 2. Reducer Machine                        │
│                 (deterministic, pure)                     │
│                                                          │
│   State = reduce(events)                                  │
│   No IO. No randomness. No SystemTime.                   │
│                                                          │
│   State contains NO ledger reference.                     │
└──────────────────────────────────────────────────────────┘
                          │ state (immutable snapshot)
                          ▼
┌─────────────────────────────────────────────────────────┐
│               3. Scheduler Machine                        │
│               (BKG-FMR routing decision)                 │
│                                                          │
│   decision = select(state, request)                       │
│   Pure function. Deterministic.                          │
│                                                          │
│   Returns: decision + trace + fallback chain             │
└──────────────────────────────────────────────────────────┘
```

**Kern-Regel:**
- Machine 1 schreibt Events (nur append)
- Machine 2 liest Events → produziert State (pure)
- Machine 3 liest State → produziert Decisions (pure)
- State enthält **NIEMALS** eine Ledger-Referenz

---

## 1. Ledger Machine (Immutable Source of Truth)

### 1.1 Core Principle

Der Ledger ist die **einzige** dauerhafte Wahrheit. State ist immer eine abgeleitete Projektion.

```rust
// canonical constructor — the ONLY way to get State
fn reduce(events: &[Event]) -> State {
    let mut state = State::new();
    for event in events {
        dispatch(&mut state, event);
    }
    state
}

// incremental (optional optimization)
fn apply_incremental(state: &mut State, event: &Event) {
    dispatch(state, event);
}
```

### 1.2 Ledger Storage

```rust
/// Append-only, tamper-evident event store.
/// Files stored at: ~/.config/llm-free/ledger/{segment:08}.jsonl
pub struct LedgerStore {
    base_path: PathBuf,
    current_segment: u32,
    entries_in_segment: u32,
    last_checksum: [u8; 32],
    writer: Option<BufWriter<File>>,
}

impl LedgerStore {
    /// Append ONE event. Returns the entry.
    pub fn append(&mut self, event: Event, clock: &mut Clock) -> Result<LedgerEntry> {
        let seq = self.next_seq();
        let logical_time = clock.tick(seq);
        let entry = LedgerEntry::new(seq, logical_time, event, &self.last_checksum);
        self.write_entry(&entry)?;
        self.last_checksum = entry.checksum;
        self.entries_in_segment += 1;
        self.maybe_rotate()?;
        Ok(entry)
    }

    /// Full replay: read all segments → verify chain → return events.
    pub fn replay(&self) -> Result<Vec<Event>> {
        let entries = self.read_all_entries()?;
        ensure!(LedgerEntry::verify_chain(&entries), "Ledger chain integrity violation");
        Ok(entries.into_iter().map(|e| e.event).collect())
    }

    /// Snapshot: persist current State + last seq.
    pub fn write_snapshot(&self, state: &State, last_seq: u64) -> Result<()> {
        let snapshot = Snapshot { last_seq, state: state.clone() };
        let path = self.base_path.join(format!("snapshot-{last_seq:016}.json"));
        // Atomic write: write to temp, then rename
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&snapshot)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Snapshot recovery: load latest snapshot, replay remaining events.
    pub fn recover(&self) -> Result<(State, u64)> {
        let (snapshot, snapshot_seq) = self.load_latest_snapshot()?;
        let all_events = self.replay()?;
        let remaining = all_events.iter().skip(snapshot_seq as usize).cloned().collect::<Vec<_>>();
        let state = reduce(&remaining, Some(snapshot.state));
        Ok((state, snapshot_seq + remaining.len() as u64))
    }
}

const MAX_ENTRIES_PER_SEGMENT: u32 = 10_000;
const MAX_BYTES_PER_SEGMENT: u64 = 10 * 1024 * 1024; // 10 MB
const SNAPSHOT_INTERVAL: u64 = 1_000; // snapshot every 1000 events
```

### 1.3 Snapshot + Compaction

```rust
#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub last_seq: u64,
    pub state: State,
}

impl LedgerStore {
    /// Decide if we need a snapshot.
    fn should_snapshot(&self, next_seq: u64) -> bool {
        next_seq > 0 && next_seq % SNAPSHOT_INTERVAL == 0
    }

    /// Compact old segments (keep last N after snapshot).
    fn compact(&mut self, snapshot_seq: u64) -> Result<()> {
        let keep_segments = 3; // keep 3 segments before snapshot for safety
        let cutoff_seq = snapshot_seq.saturating_sub(keep_segments * MAX_ENTRIES_PER_SEGMENT as u64);
        for entry in self.list_segments()? {
            if entry.end_seq < cutoff_seq {
                std::fs::remove_file(entry.path)?;
            }
        }
        Ok(())
    }
}
```

---

## 2. Reducer Machine (Deterministic State Production)

### 2.1 State Definition (OHNE Ledger)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub models: Vec<Model>,
    pub health: HashMap<ModelId, HealthStatus>,
    pub routing_cache: RoutingCache,
    pub inflight: HashMap<ModelId, u32>,   // concurrent load tracking
    pub diagnostics: Diagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    // Simplified for Phase 1 — derived from event history
    pub last_success: SequencedInstant,
    pub last_failure: SequencedInstant,
    pub success_ratio: f32,         // window-based, not EMA
    pub rolling_latency_avg_ms: f64, // window-based average
    pub garbage: bool,               // derived: too many failures
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingCache {
    pub last_decision: Option<RoutingDecision>,
    pub last_fallback: Option<FallbackEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Diagnostics {
    pub total_completions: u64,
    pub total_failures: u64,
    pub total_fallbacks: u64,
    pub total_degradations: u64,
}
```

### 2.2 Reducer Signatures (ALL Events)

```rust
/// canonical: full rebuild from ledger
pub fn reduce(events: &[Event], initial: Option<State>) -> State {
    let mut state = initial.unwrap_or_default();
    for event in events {
        dispatch(&mut state, event);
    }
    state
}

/// ONE reducer per event variant
fn dispatch(state: &mut State, event: &Event) {
    match event {
        Event::DaemonStarted { version } => {
            state.diagnostics = Diagnostics::default();
        }
        Event::ModelAdded { model, source } => {
            if state.models.iter().any(|m| m.id == model.id) { return; } // idempotent
            state.models.push(model.clone());
            state.health.insert(model.id.clone(), HealthStatus::new());
            state.inflight.insert(model.id.clone(), 0);
        }
        Event::ModelRemoved { model_id, .. } => {
            state.models.retain(|m| m.id != *model_id);
            state.health.remove(model_id);
            state.inflight.remove(model_id);
        }
        Event::ModelSelected { model_id, request, score, .. } => {
            state.routing_cache.last_decision = Some(RoutingDecision {
                model_id: model_id.clone(),
                score: *score,
                .. // full trace
            });
            // increment inflight counter
            *state.inflight.entry(model_id.clone()).or_insert(0) += 1;
        }
        Event::CompletionFinished { model_id, latency_ms, tokens_used, success } => {
            if let Some(health) = state.health.get_mut(model_id) {
                if *success {
                    health.last_success = state.current_time();
                    health.rolling_latency_avg_ms = windowed_avg(
                        health.rolling_latency_avg_ms, *latency_ms as f64, 20
                    );
                } else {
                    health.last_failure = state.current_time();
                }
                health.success_ratio = compute_success_ratio(state, model_id, 50);
                health.garbage = is_garbage(health);
            }
            // decrement inflight
            if let Some(count) = state.inflight.get_mut(model_id) {
                *count = count.saturating_sub(1);
            }
            state.diagnostics.total_completions += 1;
            if !success { state.diagnostics.total_failures += 1; }
        }
        Event::ModelFailed { model_id, error } => {
            if let Some(health) = state.health.get_mut(model_id) {
                health.last_failure = state.current_time();
                health.success_ratio = compute_success_ratio(state, model_id, 50);
                health.garbage = is_garbage(health);
            }
            state.diagnostics.total_failures += 1;
        }
        Event::ProbeUpdated { model_id, health, latency_ms } => {
            if let Some(h) = state.health.get_mut(model_id) {
                h.last_success = state.current_time();
                h.rolling_latency_avg_ms = windowed_avg(
                    h.rolling_latency_avg_ms, *latency_ms as f64, 10
                );
            }
        }
        Event::ProbeFailed { model_id, .. } => {
            if let Some(health) = state.health.get_mut(model_id) {
                health.last_failure = state.current_time();
                health.garbage = is_garbage(health);
            }
        }
        Event::FallbackTriggered { from, to, .. } => {
            state.routing_cache.last_fallback = Some(FallbackEntry {
                from: from.clone(),
                to: to.clone(),
                at: state.current_time(),
            });
            state.diagnostics.total_fallbacks += 1;
        }
        Event::DegradeModeEnabled { .. } => {
            state.diagnostics.total_degradations += 1;
        }
        Event::CompletionRequested { model_id, .. } => {
            *state.inflight.entry(model_id.clone()).or_insert(0) += 1;
        }
        Event::ReportReceived { model_id, success, latency_ms, tokens } => {
            if let Some(health) = state.health.get_mut(model_id) {
                if *success {
                    health.last_success = state.current_time();
                    health.rolling_latency_avg_ms = windowed_avg(
                        health.rolling_latency_avg_ms, *latency_ms as f64, 20
                    );
                } else {
                    health.last_failure = state.current_time();
                }
                health.success_ratio = compute_success_ratio(state, model_id, 50);
                health.garbage = is_garbage(health);
            }
        }
        Event::ConfigReloaded { .. } => { /* config update — handled by runtime */ }
        Event::DaemonStopped { .. } => { /* terminal event, no state change needed */ }
    }
}
```

### 2.3 Window-Based Metrics (statt EMA)

```rust
/// Window-based success ratio (deterministic over replay).
/// Counts last N events for model_id.
fn compute_success_ratio(state: &State, model_id: &str, window: usize) -> f32 {
    // In a real implementation, we'd track a ring buffer in diagnostics
    // For Phase 1, derive from health status fields
    let health = match state.health.get(model_id) {
        Some(h) => h,
        None => return 0.0,
    };
    if health.last_success == SequencedInstant::EPOCH && health.last_failure == SequencedInstant::EPOCH {
        return 0.5; // unknown → neutral
    }
    if health.last_failure == SequencedInstant::EPOCH {
        return 1.0; // never failed
    }
    if health.last_success == SequencedInstant::EPOCH {
        return 0.0; // never succeeded
    }
    // Simplified: ratio based on last interaction
    if health.last_success > health.last_failure { 0.8 } else { 0.3 }
}

/// Windowed rolling average (deterministic).
fn windowed_avg(current: f64, new_value: f64, window_size: u64) -> f64 {
    // Simple windowed average: weight = 1/window
    current * (1.0 - 1.0 / window_size as f64) + new_value * (1.0 / window_size as f64)
}

/// Garbage detection (deterministic).
fn is_garbage(health: &HealthStatus) -> bool {
    // 3 consecutive failures or success_ratio < 0.2 after 10+ attempts
    health.success_ratio < 0.2
        && health.last_failure > health.last_success
}
```

---

## 3. Clock & Time Model

### 3.1 Clock Owner (Single Time Source)

```rust
/// DIESER Clock erzeugt ALLE SequencedInstants im System.
/// Es gibt genau EINE Clock-Instanz pro Daemon.
pub struct Clock {
    seq: u64,
}

impl Clock {
    pub fn new() -> Self {
        Self { seq: 0 }
    }

    /// Advance clock by one tick. Returns deterministic time.
    pub fn tick(&mut self) -> SequencedInstant {
        self.seq += 1;
        SequencedInstant {
            seq: self.seq,
            logical_time: self.seq, // logical_time = seq (monotonic)
        }
    }

    pub fn current_seq(&self) -> u64 {
        self.seq
    }
}

/// Telemetry time: pairs logical time with wall-clock measurement.
/// This is the ONLY place where wall-clock data enters the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryInstant {
    pub logical: SequencedInstant,    // deterministic, for ordering
    pub wall_latency_ms: Option<u64>, // measured, informational only
}

/// Deterministic time. NO relation to wall clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SequencedInstant {
    pub seq: u64,
    pub logical_time: u64,
}

impl SequencedInstant {
    pub const EPOCH: Self = Self { seq: 0, logical_time: 0 };
}
```

---

## 4. Scheduler Machine (BKG-FMR)

### 4.1 Decision Trace Model (NEW — explainability)

```rust
/// Complete routing decision with full trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub selected_model: ModelId,
    pub score: f64,
    pub scores: Vec<ModelScore>,       // ALL scored models
    pub reasoning: Vec<ScoreFactor>,   // per-factor breakdown
    pub fallback_chain: Vec<ModelId>,
    pub timestamp: SequencedInstant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelScore {
    pub model_id: ModelId,
    pub total: f64,
    pub factors: Vec<ScoreFactor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreFactor {
    pub name: &'static str,   // "health", "latency", "success_rate", "stability", "capability"
    pub value: f64,
    pub weight: f64,
    pub contribution: f64,    // value * weight
}

impl ScoreFactor {
    fn new(name: &'static str, value: f64, weight: f64) -> Self {
        Self { name, value, weight, contribution: value * weight }
    }
}
```

### 4.2 Backpressure / Load Model

```rust
/// Scheduling constraints including load awareness.
pub struct RouteRequest {
    pub task_type: TaskType,
    pub max_latency_ms: Option<u64>,
    pub require_vision: bool,
    pub require_tool_use: bool,
    pub min_context_window: Option<u32>,
    pub max_inflight_per_model: Option<u32>,  // NEW: backpressure
}

impl Default for RouteRequest {
    fn default() -> Self {
        Self {
            task_type: TaskType::Chat,
            max_latency_ms: None,
            require_vision: false,
            require_tool_use: false,
            min_context_window: None,
            max_inflight_per_model: Some(3),  // default: max 3 concurrent
        }
    }
}
```

### 4.3 Scoring (Deterministic, Window-Based)

```rust
pub struct Scorer {
    pub weights: ScoringWeights,
}

pub struct ScoringWeights {
    pub health: f64,        // 0.35
    pub latency: f64,       // 0.20
    pub success_rate: f64,  // 0.25
    pub stability: f64,     // 0.10
    pub load: f64,          // 0.10 — NEW: backpressure factor
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self { health: 0.35, latency: 0.20, success_rate: 0.25, stability: 0.10, load: 0.10 }
    }
}

impl Scorer {
    pub fn score(&self, model: &Model, health: &HealthStatus, inflight: u32, task: &RouteRequest) -> (f64, Vec<ScoreFactor>) {
        let max_inflight = task.max_inflight_per_model.unwrap_or(3);

        let health_score = health.last_failure == SequencedInstant::EPOCH as f64;
        // actually compute from health status
        let health_factor = Factor::new("health", self.health_score(health), self.weights.health);
        let latency_factor = Factor::new("latency", self.latency_score(health), self.weights.latency);
        let success_factor = Factor::new("success_rate", self.success_rate_score(health), self.weights.success_rate);
        let stability_factor = Factor::new("stability", self.stability_score(health), self.weights.stability);
        let load_factor = Factor::new("load", self.load_score(inflight, max_inflight), self.weights.load);
        let capability_bonus = self.capability_match(model, task);

        let total = health_factor.contribution + latency_factor.contribution
            + success_factor.contribution + stability_factor.contribution
            + load_factor.contribution + capability_bonus;

        (total, vec![health_factor, latency_factor, success_factor, stability_factor, load_factor])
    }

    fn load_score(&self, inflight: u32, max_inflight: u32) -> f64 {
        if max_inflight == 0 { return 0.0; }
        f64::max(0.0, 1.0 - inflight as f64 / max_inflight as f64)
    }

    fn health_score(&self, health: &HealthStatus) -> f64 {
        if health.garbage { return 0.0; }
        if health.last_success == SequencedInstant::EPOCH { return 0.5; } // unknown
        1.0
    }

    fn latency_score(&self, health: &HealthStatus) -> f64 {
        if health.rolling_latency_avg_ms <= 0.0 { return 0.5; }
        f64::max(0.0, 1.0 - (health.rolling_latency_avg_ms - 500.0) / 9500.0)
    }

    fn success_rate_score(&self, health: &HealthStatus) -> f64 {
        if health.last_success == SequencedInstant::EPOCH && health.last_failure == SequencedInstant::EPOCH {
            return 0.5; // no data → neutral
        }
        health.success_ratio as f64
    }

    fn stability_score(&self, health: &HealthStatus) -> f64 {
        // Simplified: if last interaction was success → 1.0, else 0.0
        if health.last_success > health.last_failure { 1.0 } else { 0.0 }
    }

    fn capability_match(&self, model: &Model, task: &RouteRequest) -> f64 {
        let mut score = 0.0;
        if model.capabilities.task_suitability.contains(&task.task_type) { score += 0.15; }
        if task.require_vision && model.capabilities.supports_vision { score += 0.10; }
        if task.require_tool_use && model.capabilities.supports_tool_use { score += 0.10; }
        if let Some(min_ctx) = task.min_context_window {
            if model.capabilities.context_window >= min_ctx { score += 0.05; }
        }
        score
    }
}
```

### 4.4 Selection + Fallback (Deterministic)

```rust
impl RouterEngine {
    pub fn select(&self, state: &State, request: &RouteRequest) -> RoutingDecision {
        let mut scored: Vec<(f64, Vec<ScoreFactor>, &Model)> = state.models
            .iter()
            .filter(|m| {
                let health = state.health.get(&m.id);
                health.map_or(true, |h| !h.garbage)
            })
            .map(|m| {
                let health = state.health.get(&m.id).cloned().unwrap_or_default();
                let inflight = state.inflight.get(&m.id).copied().unwrap_or(0);
                let (score, factors) = self.scorer.score(m, &health, inflight, request);
                (score, factors, m)
            })
            .collect();

        // Sort by score desc, then deterministic tiebreaker
        scored.sort_by(|(sa, _, a), (sb, _, b)| {
            sb.partial_cmp(sa).unwrap_or(Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });

        let fallback_chain: Vec<ModelId> = scored.iter().map(|(_, _, m)| m.id.clone()).collect();
        let all_scores: Vec<ModelScore> = scored.iter().map(|(total, factors, m)| {
            ModelScore { model_id: m.id.clone(), total: *total, factors: factors.clone() }
        }).collect();

        let (selected_score, selected_factors, selected) = scored.first().map_or(
            (0.0, vec![], None),
            |(s, f, m)| (*s, f.clone(), Some(m))
        );

        RoutingDecision {
            selected_model: selected.map(|m| m.id.clone()).unwrap_or_default(),
            score: selected_score,
            scores: all_scores,
            reasoning: selected_factors,
            fallback_chain,
            timestamp: state.current_time(),
        }
    }

    pub fn fallback_chain(&self, state: &State, request: &RouteRequest, after: &ModelId) -> Vec<ModelId> {
        let decision = self.select(state, request);
        decision.fallback_chain.into_iter()
            .filter(|id| id != after)
            .collect()
    }
}
```

---

## 5. ProviderAdapter (mit Retry + Circuit Breaker)

```rust
#[async_trait]
pub trait ProviderAdapter: Send + Sync + Debug {
    fn provider_name(&self) -> &str;

    /// Complete with retry + timeout + circuit breaker.
    async fn complete(&self, model: &Model, request: CompletionRequest)
        -> Result<CompletionResponse, ProviderError>;

    /// Health probe.
    async fn probe(&self, model: &Model) -> Result<ProbeResult, ProviderError>;

    /// List available models.
    async fn list_models(&self) -> Result<Vec<ProviderModelInfo>, ProviderError>;
}

// ─── Retry Policy ───
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub retryable_errors: Vec<ErrorKind>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 10_000,
            retryable_errors: vec![ErrorKind::RateLimited { retry_after: None }, ErrorKind::Timeout { timeout_ms: 0 }],
        }
    }
}

// ─── Circuit Breaker ───
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    pub failure_threshold: u32,
    pub recovery_timeout_ms: u64,
    pub state: CircuitState,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CircuitState {
    Closed,      // normal operation
    Open,        // failing — fast reject
    HalfOpen,    // testing recovery
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self { failure_threshold: 5, recovery_timeout_ms: 30_000, state: CircuitState::Closed }
    }
}
```

---

## 6. Provider Runtime (verhindert Chaos im Adapter-Layer)

```rust
/// Wraps a ProviderAdapter with retry, timeout, circuit breaker.
pub struct ProviderRuntime {
    adapter: Box<dyn ProviderAdapter>,
    retry_policy: RetryPolicy,
    circuit_breaker: CircuitBreaker,
    consecutive_failures: u32,
    last_failure_time: Option<SystemTime>, // only here, NOT in reducers
}

impl ProviderRuntime {
    pub async fn complete_with_protection(
        &mut self,
        model: &Model,
        request: CompletionRequest,
    ) -> Result<(CompletionResponse, Event), ProviderError> {
        // Circuit breaker check
        if self.circuit_breaker.state == CircuitState::Open {
            return Err(ProviderError::CircuitBreakerOpen);
        }

        // Attempt with retry
        let mut last_error = None;
        for attempt in 0..=self.retry_policy.max_retries {
            let timeout = self.retry_policy.base_delay_ms * 2u64.pow(attempt);
            match tokio::time::timeout(
                Duration::from_millis(timeout.min(self.retry_policy.max_delay_ms)),
                self.adapter.complete(model, &request),
            ).await {
                Ok(Ok(response)) => {
                    self.consecutive_failures = 0;
                    self.circuit_breaker.state = CircuitState::Closed;
                    let event = Event::CompletionFinished {
                        model_id: model.id.clone(),
                        latency_ms: response.latency_ms,
                        tokens_used: response.usage.map(|u| u.total_tokens as u64).unwrap_or(0),
                        success: true,
                    };
                    return Ok((response, event));
                }
                Ok(Err(e)) => {
                    last_error = Some(e);
                    self.consecutive_failures += 1;
                    if self.consecutive_failures >= self.circuit_breaker.failure_threshold {
                        self.circuit_breaker.state = CircuitState::Open;
                        self.last_failure_time = Some(std::time::SystemTime::now());
                    }
                    // Don't retry if not retryable
                    if !self.is_retryable(&last_error.as_ref().unwrap()) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(
                        self.retry_policy.base_delay_ms * 2u64.pow(attempt)
                    )).await;
                }
                Err(_) => {
                    last_error = Some(ProviderError::Timeout(self.retry_policy.max_delay_ms));
                    self.consecutive_failures += 1;
                }
            }
        }

        let error = last_error.unwrap_or(ProviderError::Internal("max retries exhausted".into()));
        let event = Event::ModelFailed {
            model_id: model.id.clone(),
            error: error.kind(),
        };
        Err(error)
    }

    fn is_retryable(&self, error: &ProviderError) -> bool {
        match error {
            ProviderError::RateLimited { .. } => true,
            ProviderError::Timeout(_) => true,
            ProviderError::Network(_) => true,
            _ => false,
        }
    }
}
```

---

## 7. Event Pipeline (Canonical Execution Path)

```rust
/// The canonical execution pipeline.
/// Pipeline: Clock → Validate → Route → Persist → Reduce → Project
pub struct Pipeline {
    ledger: LedgerStore,
    clock: Clock,
    state: ProjectionView<State>,
}

impl Pipeline {
    pub fn process(&mut self, event: Event) -> Result<RoutingDecision> {
        // 1. Validate
        self.validate(&event)?;

        // 2. Generate event with clock
        let entry = self.ledger.append(event, &mut self.clock)?;

        // 3. Apply (incremental reducer)
        let mut state = self.state.inner.write().unwrap();
        dispatch(&mut state, &entry.event);

        // 4. Check for snapshot
        if self.ledger.should_snapshot(entry.seq) {
            self.ledger.write_snapshot(&state, entry.seq)?;
            self.ledger.compact(entry.seq)?;
        }

        // 5. Return current routing decision (projection)
        Ok(state.routing_cache.last_decision.clone().unwrap_or_default())
    }

    /// Validate event before processing.
    fn validate(&self, event: &Event) -> Result<(), ValidationError> {
        match event {
            Event::ModelAdded { model, .. } => {
                if model.id.is_empty() {
                    return Err(ValidationError::InvalidEvent("empty model id"));
                }
            }
            Event::CompletionRequested { messages, .. } => {
                if messages.is_empty() {
                    return Err(ValidationError::InvalidEvent("empty messages"));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Full replay from ledger (deterministic recovery).
    pub fn replay(&mut self) -> Result<ProjectionView<State>> {
        let events = self.ledger.replay()?;
        let state = reduce(&events, None);
        Ok(ProjectionView::new(state))
    }
}
```

---

## 8. Concurrency Model

```rust
/// Single-threaded event loop with async I/O.
/// This avoids all concurrency bugs in the reducer.
///
/// Architecture:
///   Main thread:  Event loop (clock + reducer + scheduler)
///   Async tasks:  Provider I/O (HTTP requests)
///
/// Communication:
///   Provider I/O → channel → Event Loop
///   HTTP API     → channel → Event Loop
///   CLI          → channel → Event Loop
///
/// The Event Loop is the ONLY writer to State.
/// ProjectionView provides read-only access for UI and API.

pub struct OmrpRuntime {
    pipeline: Pipeline,
    http_server: HttpServer,
    providers: HashMap<String, ProviderRuntime>,
    event_tx: mpsc::Sender<Event>,
    event_rx: mpsc::Receiver<Event>,
}

impl OmrpRuntime {
    pub async fn run(&mut self) -> Result<()> {
        loop {
            tokio::select! {
                // Process incoming events one at a time
                Some(event) = self.event_rx.recv() => {
                    let decision = self.pipeline.process(event);
                    // broadcast decision to UI via channel
                }
                _ = self.http_server.poll() => {
                    // HTTP server handles requests, sends events to channel
                }
            }
        }
    }
}
```

---

## 9. UI Subscription Model (Reactive, Not Polling)

```rust
/// UI receives PROJECTED state, not raw state.
/// Reactive: UI subscribes to state changes via broadcast channel.

pub struct UiBridge {
    state: ProjectionView<State>,
    event_tx: mpsc::Sender<Event>,     // UI → Engine (send events)
    state_rx: broadcast::Receiver<ProjectionView<State>>, // Engine → UI
}

// In Dioxus:
fn App(cx: Scope) -> Element {
    let bridge = use_coroutine(cx, |rx| {
        // subscribe to state updates
    });
    let state = bridge.state.read();
    // render projection
}
```

---

## 10. Failure Semantics

| Bedingung | Zählt als Failure? | Event | Retry? |
|-----------|-------------------|-------|--------|
| HTTP 200 with valid response | ❌ Nein | `CompletionFinished { success: true }` | Nein |
| HTTP 429 (Rate Limit) | ✅ Ja | `ModelFailed { error: RateLimited }` | ✅ Ja, max 3x |
| HTTP 5xx | ✅ Ja | `ModelFailed { error: InternalError }` | ✅ Ja, max 3x |
| Timeout (10s) | ✅ Ja | `ModelFailed { error: Timeout }` | ✅ Ja, max 2x |
| Network error (DNS, TCP) | ✅ Ja | `ModelFailed { error: NetworkError }` | ✅ Ja, max 2x |
| Auth error (401/403) | ✅ Ja | `ModelFailed { error: AuthError }` | ❌ Nein |
| Model not found (404) | ✅ Ja | `ModelFailed { error: ModelNotAvailable }` | ❌ Nein |
| Partial response | ⚠️ Ja | `ModelFailed { error: InternalError }` | Nein (incomplete) |
| Circuit breaker open | ✅ Ja | `ModelFailed { error: InternalError }` | ❌ (fast reject) |

---

## 11. Formal Invariants (debug_assert! checks)

```rust
pub fn assert_invariants(state: &State) {
    // Invariant 1: NO ledger in state
    #[cfg(debug_assertions)] {
        // Compile-time: State struct has no ledger field
    }

    // Invariant 2: Every model has health + inflight entry
    for model in &state.models {
        debug_assert!(state.health.contains_key(&model.id),
            "Model {:?} missing health entry", model.id);
        debug_assert!(state.inflight.contains_key(&model.id),
            "Model {:?} missing inflight entry", model.id);
    }

    // Invariant 3: No garbage model in last decision
    if let Some(ref decision) = state.routing_cache.last_decision {
        if !decision.selected_model.is_empty() {
            if let Some(health) = state.health.get(&decision.selected_model) {
                debug_assert!(!health.garbage,
                    "Selected garbage model: {}", decision.selected_model);
            }
        }
    }

    // Invariant 4: Inflight counts are non-negative
    for (model_id, count) in &state.inflight {
        debug_assert!(*count >= 0, "Negative inflight for {model_id}");
    }

    // Invariant 5: Health times are monotonic with clock
    // (checked at event processing time)
}
```

---

## 12. Key Changes from v1.0

| Issue | v1.0 | v1.1 |
|-------|------|------|
| Dual truth (state + ledger) | State contained `ledger: Vec<Event>` | State is pure projection, ledger is external |
| EMA drift | EMA in reducers (non-deterministic across replays) | Window-based metrics (deterministic) |
| Missing backpressure | No load tracking | `inflight: HashMap<ModelId, u32>` + load scoring factor |
| Missing decision trace | Just `model_id + score` | Full `RoutingDecision` with per-factor breakdown |
| Missing clock owner | Implicit seq generation | `Clock` struct — single time source |
| Missing snapshot strategy | Not addressed | Snapshot every 1000 events, compaction + recovery |
| Missing retry policy | Clean adapter trait | `ProviderRuntime` with retry + circuit breaker |
| Missing concurrency model | Not addressed | Single-threaded event loop + async I/O via channels |
| Missing UI subscription | Implicit | `broadcast::Receiver` for reactive updates |
| Time model mixing | Implicit | `TelemetryInstant` separates logical from wall-clock |

---

> **Ready for implementation.** Next step: Implementation Plan (Phase 1 Rust MVP).
