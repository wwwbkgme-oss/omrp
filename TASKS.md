# OMRP — Next Tasks

> Phase 1 (kernel bootstrapping) is complete.
> This file tracks Phase 2 and beyond.
>
> Status notation: `- [ ]` pending, `- [x]` done, `- [~]` in progress

---

## Phase 1 — Kernel Bootstrapping ✓ Complete

- [x] `omrp-types` — `SequencedInstant`, `Clock`, `Model`, `TaskType`, `RoutingDecision`
- [x] `omrp-events` — `Event` enum (14 variants), `ErrorKind`, `validate()`
- [x] `omrp-core/state` — `State`, `HealthStatus`, `Diagnostics`
- [x] `omrp-core/reducers` — `dispatch()`, EMA success ratio, garbage detection
- [x] `omrp-core/pipeline` — `EventPipeline` with validate → persist → apply → replay
- [x] `omrp-core/scorer` — BKG-FMR 5-factor scoring with capability bonus
- [x] `omrp-core/router` — `RouterEngine` with deterministic selection + fallback chain
- [x] `omrp-core/ledger` — `LedgerStore` with SHA-256 chaining, JSON Lines persist/load
- [x] `omrp-runtime` — CLI: `models`, `status`, `best <task>`
- [x] Integration tests — determinism, replay identity, fuzz (34 tests, 0 failures)
- [x] Detailed documentation — `ARCHITECTURE.md`, `EVENTS.md`, `ROUTING.md`

---

## Phase 2 — Persistence & Config

**Goal:** Replace the demo bootstrap with a real persistent event log and
config-driven model registry. No provider HTTP calls yet.

### 2.1 Wire LedgerStore into EventPipeline

The `EventPipeline` currently stores events in an in-memory `Vec<Event>`.
Replace it with the `LedgerStore` for durable, tamper-evident persistence.

- [ ] Add `LedgerStore` field to `EventPipeline`
- [ ] On `process()`: call `ledger.append(event)` before `dispatch()`
- [ ] On startup: call `LedgerStore::load(path)` and replay to rebuild `State`
- [ ] Add `EventPipeline::load_from_ledger(path) -> Result<Self, LedgerError>`
- [ ] Test: persist → restart → reload → state equals original

**Files:** `crates/omrp-core/src/pipeline.rs`, `crates/omrp-core/src/ledger.rs`

---

### 2.2 Config File Loading

Allow users to define their model registry in a config file instead of
hard-coding models in `main.rs`.

- [ ] Define config schema (TOML preferred):
  ```toml
  [[model]]
  id = "openrouter/claude-3-5-sonnet"
  provider = "openrouter"
  context_window = 200000
  supports_tool_use = true
  tasks = ["code", "reasoning", "chat"]
  ```
- [ ] Add `omrp-config` crate (or module in `omrp-runtime`) with a `Config` struct
- [ ] Parse config and emit `ModelAdded` events into the pipeline on startup
- [ ] Watch config file for changes → emit `ConfigReloaded` + `ModelAdded`/`ModelRemoved` as needed
- [ ] Default config path: `~/.config/omrp/config.toml` (XDG-compliant)
- [ ] CLI flag: `omrp --config <path>`
- [ ] Test: load config, verify models are registered in state

**Files:** `crates/omrp-runtime/src/main.rs`, new `crates/omrp-config/` (optional)

**Dependency to add:** `toml = "0.8"`, `dirs = "5"`

---

### 2.3 Ledger Segmentation

The current `LedgerStore` rewrites the entire file on every `persist()`.
For long-running daemons this becomes expensive.

- [ ] Add segment-based storage: `ledger/00000001.jsonl`, `ledger/00000002.jsonl`, ...
- [ ] Auto-rotate when a segment exceeds `MAX_SEGMENT_ENTRIES` (e.g. 10 000)
- [ ] `LedgerStore::load_all(base_path)` reads all segments in order and verifies the cross-segment chain
- [ ] Add `LedgerStore::compact(keep_last_n)` to trim old segments
- [ ] Test: rotate mid-append, reload across segments, verify chain integrity

**Files:** `crates/omrp-core/src/ledger.rs`

---

## Phase 3 — Provider Adapters

**Goal:** Make real HTTP calls to LLM providers and emit completion events.

### 3.1 `ProviderAdapter` Trait

- [ ] Define `ProviderAdapter` trait in `omrp-core` (or new `omrp-providers` crate):
  ```rust
  pub trait ProviderAdapter: Send + Sync {
      fn provider_id(&self) -> &str;
      fn complete(&self, model_id: &ModelId, prompt: &str,
                  task_type: TaskType) -> Result<CompletionResult, ProviderError>;
  }
  ```
- [ ] `CompletionResult { text: String, tokens_used: u64, latency_ms: u64 }`
- [ ] Emit `CompletionRequested` before call, `CompletionFinished` or `ModelFailed` after
- [ ] No async in Phase 3 (blocking calls, matches plan spec)

---

### 3.2 OpenRouter Adapter

- [ ] `OpenRouterAdapter { api_key: String, base_url: String }`
- [ ] `POST /api/v1/chat/completions` (OpenAI-compatible format)
- [ ] Read API key from `OPENROUTER_API_KEY` environment variable
- [ ] Map HTTP errors to `ProviderError` variants
- [ ] Handle rate limit headers (`Retry-After`) → `ProviderError::RateLimited`
- [ ] Test with mock HTTP server

**Dependency:** `ureq = "2"` (blocking, no tokio needed)

---

### 3.3 Retry Policy

- [ ] Define `RetryPolicy { max_attempts: u32, base_delay_ms: u64, backoff: BackoffKind }`
- [ ] `BackoffKind`: `Linear`, `Exponential { factor: f64 }`, `Fixed`
- [ ] Apply retry policy in `EventPipeline::route_with_retry()`
- [ ] Emit `FallbackTriggered` when a retry switches models
- [ ] Test: first call fails, second succeeds → correct events emitted

---

### 3.4 Circuit Breaker

The `ProviderError::CircuitBreakerOpen` variant exists — implement it.

- [ ] Add `CircuitBreakerState` to `HealthStatus` (or separate `HashMap` in `State`):
  - `Closed` (normal), `HalfOpen` (testing recovery), `Open` (rejecting calls)
- [ ] Open when `consecutive_failures >= threshold` (e.g. 5)
- [ ] Transition to `HalfOpen` after `open_duration_ms`
- [ ] Close on success in `HalfOpen`
- [ ] Emit `ModelFailed { error: CircuitBreakerOpen }` when short-circuiting
- [ ] Test: consecutive failures open the breaker, successful probe closes it

---

### 3.5 Health Probe Scheduler

- [ ] Background thread that calls a "ping" endpoint per model on a configurable interval
- [ ] Emits `ProbeUpdated` or `ProbeFailed` into the pipeline
- [ ] Per-model probe interval (default 30 s, configurable)
- [ ] Exponential backoff on repeated `ProbeFailed`
- [ ] Graceful shutdown (`SIGTERM` → drain inflight → stop probes → persist ledger)

---

## Phase 4 — HTTP Proxy / API Server

**Goal:** OMRP acts as a transparent OpenAI-compatible proxy. Clients point
their existing code at OMRP; OMRP selects the best model and forwards.

### 4.1 REST API

- [ ] `GET  /v1/models` → list registered models (OpenAI-compatible format)
- [ ] `GET  /v1/status` → current `State` snapshot as JSON
- [ ] `GET  /v1/routing/decision?task=code` → current routing decision for a task
- [ ] `POST /v1/routing/fallback` → manually trigger fallback for a model
- [ ] Embed in `omrp-runtime` using `tiny_http` or `actix-web`

---

### 4.2 OpenAI-Compatible Proxy

- [ ] `POST /v1/chat/completions` — accepts standard OpenAI request format
- [ ] Route to best model via `RouterEngine::select`
- [ ] Forward request to provider adapter
- [ ] Emit events on request/response
- [ ] Return response in OpenAI format (preserve `model` field as actual model used)
- [ ] Include `X-OMRP-Model`, `X-OMRP-Score`, `X-OMRP-Fallbacks` response headers

---

### 4.3 Streaming Support

- [ ] Support `"stream": true` in chat/completions requests
- [ ] Proxy SSE (server-sent events) stream from provider to client
- [ ] Emit `CompletionFinished` only after stream completes (or first token for latency metric)

---

## Phase 5 — Dashboard

**Goal:** Read-only Dioxus dashboard that projects `State` in real time.

- [ ] `omrp-dashboard` crate (Dioxus, WASM or desktop)
- [ ] Subscribe to `EventPipeline` state via `ProjectionView::read()`
- [ ] Live model health table (score, latency, ratio, garbage flag)
- [ ] Event log viewer (last N events from ledger)
- [ ] Routing decision history (last N `ModelSelected` events)
- [ ] No write path — purely reads from `State` projection

---

## Phase 6 — Advanced Routing

### 6.1 Cost-Aware Routing

- [ ] Add `cost_per_1k_tokens: Option<f64>` to `Model`
- [ ] Add `max_cost_per_request: Option<f64>` to `RouteRequest`
- [ ] New scoring factor: `cost_score = 1.0 - (cost / budget)`, weight TBD
- [ ] Emit `CostExceeded` event (new variant) when budget is breached

---

### 6.2 Token Budget Enforcement

- [ ] Add `token_budget` to `RouteRequest`
- [ ] Pre-flight check: if `prompt_tokens >= model.context_window`, skip model
- [ ] Track `total_tokens_used` per model in `State` (daily/hourly window)

---

### 6.3 Latency Pre-Filter

- [ ] If `RouteRequest.max_latency_ms` is set, remove models where
  `rolling_latency_avg_ms > max_latency_ms` before scoring
- [ ] Emit `DegradeModeEnabled` if no model passes the latency filter

---

### 6.4 Priority Queuing

- [ ] Add `priority: u8` (0–255) to `RouteRequest`
- [ ] High-priority requests preempt low-priority ones in the inflight counter

---

## Infrastructure / Developer Experience

- [ ] **Change default branch** from `feature/omrp-phase1` to `main` in GitHub repo settings
  (requires repo admin — cannot be automated with current token)
- [ ] **Delete `feature/omrp-phase1`** after changing default branch
- [ ] Add `cargo clippy -- -D warnings` to CI
- [ ] Add `cargo fmt --check` to CI
- [ ] GitHub Actions workflow: `on: push` → `cargo test --workspace`
- [ ] Benchmark suite: `criterion` for `dispatch()` throughput and `select()` latency
- [ ] Fuzz testing: `cargo-fuzz` target for the ledger's `load()` path

---

## Spec References

- `docs/superpowers/specs/2026-05-30-omrp-kernel-spec.md` — trait definitions, Phase 2 types
- `docs/superpowers/specs/2026-05-30-omrp-execution-semantics.md` — execution model, backpressure, time model
- `docs/superpowers/specs/2026-05-30-llm-free-protocol-design.md` — original protocol design
- `docs/superpowers/plans/2026-05-30-omrp-phase1-kernel.md` — Phase 1 implementation plan (complete)
