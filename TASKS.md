# OMRP — Next Tasks

> **Goal for this list:** get from a working demo kernel to a locally usable
> LLM router — i.e., you configure your models, set an API key, and `omrp`
> transparently routes every completion request to the best available model.
>
> Tasks are ordered by dependency.  Phase 2 must come first; later phases
> build on top of it.  Each task is sized to be one focused pull request.

---

## Phase 1 — Kernel ✅ Complete

All core machinery is implemented and tested (34 tests, 0 failures).

```
cargo run -p omrp-runtime -- best code   # already works with demo models
```

---

## Phase 2 — Config + Persistence  *(implement first)*

These two tasks make the daemon non-ephemeral.  Without them, every
restart loses all health history and model config.

### 2-A  Config file loading  `[next]`

**Why first:** users can't define their own models without editing Rust code.

**What to build:**
- TOML config at `~/.config/omrp/config.toml` (XDG-compliant, `--config` flag override)
- Config schema:
  ```toml
  [daemon]
  ledger_path = "~/.local/share/omrp/ledger"   # where to store events

  [[model]]
  id       = "openrouter/claude-3-5-sonnet"
  provider = "openrouter"
  tasks    = ["code", "reasoning", "chat", "analysis"]
  tool_use = true
  ctx      = 200_000

  [[model]]
  id       = "qwen/qwen-2-5-72b"
  provider = "qwen"
  tasks    = ["code", "chat", "reasoning"]
  ctx      = 32_768
  ```
- On startup: parse config, emit `ModelAdded` events for each model
- On `ConfigReloaded`: diff old vs new, emit `ModelAdded` / `ModelRemoved` as needed
- Replace `demo_pipeline()` in `omrp-runtime/src/main.rs` with config-driven bootstrap

**Files:** new `crates/omrp-config/` or `crates/omrp-runtime/src/config.rs`  
**Deps to add:** `toml = "0.8"`, `dirs = "5"`  
**Tests:** parse valid config → correct ModelAdded events, missing file → sensible default

---

### 2-B  Persistent LedgerStore in EventPipeline  `[next]`

**Why next:** without this, health scores reset on every restart.

**What to build:**
- `EventPipeline::new_persistent(ledger_path: PathBuf) -> Result<Self>`
  - calls `LedgerStore::load(path)` to restore history
  - replays all stored events to rebuild `State`
- Replace the in-memory `Vec<Event>` log with `LedgerStore`
- Call `ledger.persist()` after each `append` (or batch-persist every N events)
- Add `omrp status --verbose` output showing event count and last seq

**Files:** `crates/omrp-core/src/pipeline.rs`, `crates/omrp-core/src/ledger.rs`  
**Tests:** persist → restart → reload → `State` equals original (golden-file test)

---

## Phase 3 — First Real API Call  *(makes the app useful)*

After Phase 2 the daemon remembers state across restarts.
Phase 3 makes the first live completion call.

### 3-A  ProviderAdapter trait + OpenRouter adapter

**What to build:**
```rust
pub trait ProviderAdapter: Send + Sync {
    fn provider_id(&self) -> &str;
    fn complete(
        &self,
        model_id: &ModelId,
        messages: &[Message],       // OpenAI-style message list
        task_type: TaskType,
    ) -> Result<CompletionResult, ProviderError>;
}

pub struct CompletionResult {
    pub text: String,
    pub tokens_used: u64,
    pub latency_ms: u64,
}
```

- `OpenRouterAdapter { api_key: String }` — reads `OPENROUTER_API_KEY` env var
- `POST https://openrouter.ai/api/v1/chat/completions`
- Map HTTP status codes and headers to `ProviderError` variants
  (`429` + `Retry-After` → `RateLimited`, `401` → `AuthError`, etc.)
- Emit `CompletionRequested` before call, `CompletionFinished` or `ModelFailed` after
- Blocking (no tokio needed — `ureq = "2"`)

**Files:** new `crates/omrp-providers/src/openrouter.rs`  
**Deps:** `ureq = "2"`  
**Tests:** mock HTTP server (ureq supports test agents), test happy path + each error variant

---

### 3-B  `omrp route` CLI command

**What to build:**
```bash
echo "explain monads" | omrp route --task reasoning
omrp route --task code --prompt "write a fizzbuzz in Rust"
```

- Reads prompt from stdin or `--prompt`
- Calls `RouterEngine::select` → picks best model
- Calls provider adapter → prints response text to stdout
- Emits events into the persistent pipeline on success/failure
- Exit code `1` on failure (with error on stderr)

**Files:** `crates/omrp-runtime/src/main.rs`

---

### 3-C  Retry policy + automatic fallback

**What to build:**
- `RetryPolicy { max_attempts: u32, backoff_ms: u64 }` in config
- On `ProviderError::RateLimited` or `NetworkError`: wait `backoff_ms` and retry same model
- On `ProviderError::AuthError` or `ModelNotAvailable`: skip model immediately, try next in fallback chain
- Emit `FallbackTriggered { from, to, cause }` on each switch
- `CompletionFinished { success: false }` after all attempts exhausted

---

## Phase 4 — Health Probes  *(keeps scores current without manual calls)*

### 4-A  Background probe thread

**What to build:**
- Spawn one thread per registered model on startup
- Every `probe_interval_s` (default 30, configurable per model in config):
  send a minimal `POST /chat/completions` with a 1-token prompt
- Emit `ProbeUpdated { health, latency_ms }` on success
- Emit `ProbeFailed { error }` on failure
- Exponential backoff after consecutive `ProbeFailed` (cap at 5 min)
- Graceful shutdown on `SIGTERM`

---

### 4-B  `omrp status --watch`

Live table that refreshes every second using probe data:
```
Model                                  Score   Latency  Ratio  Inflight
openrouter/claude-3-5-sonnet           1.223    820ms   89.0%     0
qwen/qwen-2-5-72b                      1.076    138ms   95.2%     1
openrouter/gpt-4o                      1.053    1200ms  78.3%     0
```

---

## Phase 5 — Proxy Server  *(drop-in replacement for OpenAI endpoint)*

### 5-A  OpenAI-compatible HTTP proxy

```bash
OPENROUTER_API_KEY=sk-... omrp serve --port 8080
```

```bash
# from any OpenAI client, just change base_url:
curl http://localhost:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"auto","messages":[{"role":"user","content":"hello"}]}'
```

**What to build:**
- `POST /v1/chat/completions` — routes to best model, proxies response
- `GET  /v1/models` — lists registered models in OpenAI format
- Add response headers: `X-OMRP-Model`, `X-OMRP-Score`, `X-OMRP-Fallbacks`
- Streaming (`"stream": true`) via chunked SSE passthrough

**Files:** `crates/omrp-runtime/src/server.rs`  
**Deps:** `tiny_http = "0.12"` or `axum = "0.7"` (axum if async is acceptable)

---

### 5-B  `omrp serve` daemonise + PID file

- `omrp serve --daemonize` → background process, write `/tmp/omrp.pid`
- `omrp stop` → read PID file, send SIGTERM

---

## Phase 6 — Circuit Breaker  *(prevents hammering dead models)*

- `CircuitState` per model: `Closed → Open → HalfOpen → Closed`
- Open when `consecutive_failures >= threshold` (default 5, configurable)
- Reject calls immediately with `ProviderError::CircuitBreakerOpen` while Open
- Transition to HalfOpen after `open_duration_s` (default 60 s)
- Close on first success in HalfOpen
- Emit `ModelFailed { error: InternalError("circuit breaker open") }` when short-circuiting
- Show circuit state in `omrp status`

---

## Immediate Infrastructure  *(can be done in parallel)*

- [ ] **Change default branch** — already done by repo owner ✓  
      Delete `feature/omrp-phase1` (now possible since `main` is default): `git push origin --delete feature/omrp-phase1`
- [ ] **GitHub Actions CI** — `.github/workflows/ci.yml`:
  ```yaml
  on: [push, pull_request]
  jobs:
    test:
      runs-on: ubuntu-latest
      steps:
        - uses: actions/checkout@v4
        - uses: dtolnay/rust-toolchain@stable
        - run: cargo test --workspace
        - run: cargo clippy --workspace -- -D warnings
        - run: cargo fmt --check
  ```
- [ ] **`cargo clippy` clean** — fix any existing clippy warnings (run `cargo clippy --workspace` to find them)
- [ ] **Version bump** — change `version = "0.1.0"` to `"0.2.0"` in `Cargo.toml` after Phase 2 lands

---

## Backlog  *(after the app is usable)*

- [ ] **Dioxus dashboard** — read-only projection of `State`, live refresh
- [ ] **Cost-aware routing** — `cost_per_1k_tokens` in config, `max_cost` in request
- [ ] **Token budget** — pre-filter models whose `context_window < prompt_tokens`
- [ ] **Latency pre-filter** — hard cap: skip models where avg latency > `max_latency_ms`
- [ ] **Qwen adapter** — same shape as OpenRouter but different auth/endpoint
- [ ] **`omrp replay`** — replay ledger and print state at each step (debugging tool)
- [ ] **`omrp verify`** — verify ledger chain integrity without replaying state
- [ ] **Benchmarks** — `criterion` for `dispatch()` throughput and `select()` latency

---

## Quick-Start Path (minimal to make it usable)

```
Phase 2-A  config loading           ~1 day
Phase 2-B  persistent ledger        ~1 day
Phase 3-A  OpenRouter adapter       ~1 day
Phase 3-B  omrp route command       ~0.5 day
Phase 3-C  retry + fallback         ~0.5 day
─────────────────────────────────────────
Total: ~4 days to a working local router
```

After those five tasks, `omrp` is a usable tool:
```bash
# configure once
cat > ~/.config/omrp/config.toml << 'EOF'
[[model]]
id = "openrouter/claude-3-5-sonnet"
provider = "openrouter"
tasks = ["code", "reasoning", "chat"]
tool_use = true
ctx = 200000
EOF

# route a request
OPENROUTER_API_KEY=sk-... omrp route --task code --prompt "write a Go HTTP server"
```

See `docs/ARCHITECTURE.md`, `docs/EVENTS.md`, and `docs/ROUTING.md` for
implementation details.
