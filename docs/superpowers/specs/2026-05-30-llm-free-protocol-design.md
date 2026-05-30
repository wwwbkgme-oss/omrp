# BKG-FMR + OMRP — Event-Sourced LLM Routing Engine

> **Status:** Draft v4 — Rust event-sourced architecture
> **Product Name:** BKG-FMR (Best Known Garbage-Free Models Router)
> **Protocol Name:** OMRP (Open Model Routing Protocol)
> **Architekturklasse:** Event-sourced distributed decision engine (Kubernetes Scheduler, CRDT-Systeme)

---

## Was das ist

Kein „LLM Router" mehr. Sondern ein **event-sourced, deterministischer, replay-sicherer LLM Execution Scheduler** in Rust, mit Dioxus Dashboard als reiner Projektions-UI.

---

## Kernarchitektur

```
┌──────────────────────────────────────┐
│          Dioxus Dashboard             │
│   (Projection-only UI Layer)          │
│   liest: ProjectionView<State>        │
└──────────────┬────────────────────────┘
               │ read-only
┌──────────────▼────────────────────────┐
│          OMRP Core Engine (Rust)       │
│                                        │
│  ┌──────────────────────────────────┐  │
│  │        Event Pipeline             │  │
│  │  process(Event) → Ledger write   │  │
│  └──────────┬───────────────────────┘  │
│             │                          │
│  ┌──────────▼───────────────────────┐  │
│  │    State Transition Layer         │  │
│  │  StateTransitionFn<E> only       │  │
│  └──────────┬───────────────────────┘  │
│             │                          │
│  ┌──────────▼───────────────────────┐  │
│  │    BKG-FMR Routing Engine         │  │
│  │  - select model                   │  │
│  │  - fallback chain                 │  │
│  │  - scoring                        │  │
│  └──────────┬───────────────────────┘  │
│             │                          │
│  ┌──────────▼───────────────────────┐  │
│  │    Provider Adapters              │  │
│  │  OpenRouter / Qwen / Kilo        │  │
│  └──────────────────────────────────┘  │
└────────────────────────────────────────┘
               │ HTTP/STDIO
               ▼
         CLI / External Agents
```

---

## System Invarianten (hart formalisiert)

### 🔒 Single Source of Truth

Ein Konzept = eine Crate:

```
crates/
├── omrp-core/          # routing + state
├── omrp-events/        # event definitions
├── omrp-runtime/       # daemon + execution
├── omrp-providers/     # adapters
└── omrp-ui/            # dioxus dashboard
```

Kein Konzept wird gesplittet. Kein „shared utils hell".

### 🔒 Single Mutation Path

Jeder State-Change MUSS durch:

```rust
fn apply(state: &mut State, event: Event)
```

oder funktional:

```rust
type StateTransitionFn<E> = fn(&mut State, E);
```

Kein `state.models.push()` direkt. Kein direkter Struct-Mutation-Access.

### 🔒 Event Pipeline Pflicht

Alles geht durch:

```rust
EventPipeline::process(event)
```

Pipeline: `Validate → Route → Persist → Apply → Project`

### 🔒 Replay Safety (CRITICAL)

Same ledger + same reducers = identical state.

- Keine Randomness
- Keine hidden time
- Keine external mutation
- Keine implicit IO in Reducern

### 🔒 No SystemTime::now()

Ersetzt durch:

```rust
struct SequencedInstant {
    seq: u64,
    logical_time: u64,
}
```

Zeit = deterministisch, nicht real-world.

### 🔒 Projection-only UI (Dioxus)

UI darf NICHT:
- State mutieren
- Events direkt erzeugen
- Engine beeinflussen

UI darf:
- Events senden (über definierte Channels)
- Views rendern (read-only ProjectionView)

---

## BKG-FMR Routing Engine (Core Idea)

**Best Known Garbage-Free Models Router**

Bevorzugt stabile Modelle. Verwirft noisy Endpoints. Minimiert Latenz-Varianz. Optimiert Success-Rate.

### Routing Score (formal)

```
score(model, task) =
    w_health * health(model)
  + w_latency * latency(model)
  + w_success * success_rate(model)
  + w_stability * stability_index(model)
```

### Fallback State Machine

```
SELECT → EXECUTE → SUCCESS ✓
                 ↘ FAIL → NEXT MODEL
                          ↘ FAIL → DEGRADE
                                   ↘ REPORT
```

---

## Event System

```rust
enum Event {
    ModelSelected { model_id: String },
    CompletionRequested { prompt: String, task_type: TaskType },
    CompletionFinished { model_id: String, latency_ms: u64, tokens: u64 },
    ModelFailed { model_id: String, error: ErrorKind },
    ProbeUpdated { model_id: String, health: f32, latency_ms: u64 },
    ProbeFailed { model_id: String, error: String },
    ModelAdded { model: Model },
    ModelRemoved { model_id: String },
    FallbackTriggered { from: String, to: String, reason: String },
    DegradeMode { model_id: String, reason: String },
    ConfigReloaded { source: String },
    ReportReceived { model_id: String, success: bool, latency_ms: u64, tokens: u64 },
}
```

### Event Pipeline (strikte Regel)

```rust
fn process(event: Event) -> Result<State, Error> {
    validate(&event)?;                     // Validate
    let state = apply_transition(state, event); // Route + Apply
    persist(event)?;                       // Persist (Ledger)
    Ok(project(state))                     // Project
}
```

---

## State Model

```rust
struct State {
    models: Vec<Model>,
    health: HashMap<ModelId, HealthStatus>,
    ledger: Vec<Event>,
    routing_cache: RoutingCache,
    config: Config,
}

struct HealthStatus {
    score: f32,
    last_probe: SequencedInstant,
    last_success: SequencedInstant,
    success_rate: f32,
    avg_latency_ms: f64,
    stability_index: f32,
    consecutive_failures: u32,
}
```

---

## Dioxus Dashboard (Projection-only UI)

```rust
fn dashboard(state: ProjectionView<State>) -> Element {
    // read-only rendering
}
```

Views:
- **Model Health Grid** — live health scores aller Modelle
- **Active Routing Decision** — aktuelles Routing + Begründung
- **Fallback Chain Trace** — sichtbare Fallback-Kette
- **Latency Graph** — Latenzverlauf pro Modell
- **Event Ledger Timeline** — durchlaufende Event-Historie

---

## Runtime Architecture

Eine Binary: `omrp-gateway`

| Command | Funktion |
|---------|----------|
| `omrp run` | Daemon starten (HTTP + Engine) |
| `omrp ui`  | Dioxus Dashboard öffnen |
| `omrp status` | CLI-Projektion des State |
| `omrp complete` | Direkte Execution |
| `omrp models` | Modelle listen |

---

## Crate-Struktur

```
crates/
├── omrp-core/
│   ├── src/
│   │   ├── lib.rs
│   │   ├── state.rs          # State struct + TransitionFn
│   │   ├── engine.rs         # BKG-FMR Routing Engine
│   │   ├── scorer.rs         # Scoring formula
│   │   ├── selector.rs       # Model selection + fallback
│   │   └── pipeline.rs       # EventPipeline
│   └── Cargo.toml
│
├── omrp-events/
│   ├── src/
│   │   ├── lib.rs
│   │   ├── event.rs          # Event enum
│   │   ├── validate.rs       # Event validation
│   │   └── serde.rs          # Ledger serialization
│   └── Cargo.toml
│
├── omrp-runtime/
│   ├── src/
│   │   ├── lib.rs
│   │   ├── daemon.rs         # Daemon lifecycle
│   │   ├── http.rs           # HTTP server (axum/actix)
│   │   ├── cli.rs            # CLI interface
│   │   └── config.rs         # Config loading
│   └── Cargo.toml
│
├── omrp-providers/
│   ├── src/
│   │   ├── lib.rs
│   │   ├── adapter.rs        # ProviderAdapter trait
│   │   ├── openai.rs         # OpenAI-compatible adapter
│   │   └── registry.rs       # Provider registry
│   └── Cargo.toml
│
└── omrp-ui/
    ├── src/
    │   ├── main.rs           # Dioxus entry
    │   ├── views/
    │   │   ├── health_grid.rs
    │   │   ├── routing_view.rs
    │   │   ├── latency_graph.rs
    │   │   └── ledger.rs
    │   └── projection.rs     # ProjectionView<T>
    └── Cargo.toml
```

---

## Core Traits (Rust)

```rust
// ─── Provider Adapter ───
#[async_trait]
trait ProviderAdapter: Send + Sync {
    fn provider_name(&self) -> &str;
    async fn complete(&self, model: &Model, request: CompletionRequest) -> Result<CompletionResponse>;
    async fn probe(&self, model: &Model) -> Result<ProbeResult>;
}

// ─── State Transition ───
type StateTransitionFn<E> = fn(&mut State, E);

trait EventHandler<E> {
    fn apply(&self, state: &mut State, event: E);
}

// ─── Event Pipeline ───
trait EventPipeline {
    type Event;
    type State;
    type Error;
    
    async fn process(&mut self, event: Self::Event) -> Result<ProjectionView<Self::State>, Self::Error>;
    fn replay(&mut self, events: &[Self::Event]) -> Self::State;
}

// ─── Projection ───
struct ProjectionView<T> {
    state: Arc<RwLock<T>>,
}

impl<T> ProjectionView<T> {
    fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.state.read().unwrap())
    }
}
```

---

## Ledger Format

```rust
struct LedgerEntry {
    seq: u64,
    timestamp: SequencedInstant,
    event: Event,
    checksum: [u8; 32],  // SHA-256 of previous entries + this event
}

impl LedgerEntry {
    fn verify_chain(&self, previous: &LedgerEntry) -> bool {
        // Kette verifizieren (tamper-evident)
    }
}
```

Ledger wird gespeichert als:
```
~/.config/llm-free/ledger/
├── 00000001.ledger
├── 00000002.ledger
└── ...
```

---

> **Nächster Schritt:** OMRP Kernel Spec (Rust Core) — genaue Rust Traits, Event→State Reducer Signatures, Provider Adapter Trait, Routing Determinism Rules, Ledger Format.
