# OMRP Provider Setup

OMRP supports 5 OpenAI-compatible providers.  All are permanently free-tier
(no credit card required for the default model pool).

---

## Provider Summary

| Provider | Env Var | Free tier | Best for |
|----------|---------|-----------|----------|
| **Cerebras** | `CEREBRAS_API_KEY` | 14,400 req/day | Fastest inference (wafer-scale) |
| **Groq** | `GROQ_API_KEY` | 1k–14k req/day | Ultra-low latency |
| **Kilo** | `KILO_API_KEY` | kilo/auto-free | Smart auto-router |
| **OpenRouter** | `OPENROUTER_API_KEY` | 50–1000 req/day | Largest free model pool |
| **BUW** | `BUW_API_KEY` | Virtual gateway | omrp-auto and kilo virtual models |

---

## Configuration Methods

### 1. Environment variables (simplest)

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
export KILO_API_KEY=kilo-...
export CEREBRAS_API_KEY=csk-...
export GROQ_API_KEY=gsk_...
export BUW_API_KEY=buw-...

omrp route --task code "write fibonacci in Rust"
```

### 2. Database (via dashboard — recommended for `omrp serve`)

1. Open `http://localhost:18800`
2. Go to **Admin → Provider Keys**
3. Click **+ Add Key**, select provider, paste key, set as **Global**

Global provider keys apply to all users.  Per-user keys override globals
for that specific user.

### 3. Config file

Add provider keys to `~/.config/omrp/config.toml`:

```toml
# Provider API keys are read from environment variables, not the config file.
# The config file only defines models and daemon settings.
# Use 'omrp init' to create the default config.
```

**Note:** API key values are never stored in the TOML config file.
Use environment variables or the database.

---

## Cerebras

**Sign up:** https://cloud.cerebras.ai

```bash
export CEREBRAS_API_KEY=csk-...
```

**Free tier:** 14,400 req/day, wafer-scale speed (fastest provider).

**Default models:**

| Model | Tier | Context | Tasks |
|-------|------|---------|-------|
| `llama3.1-8b` | simple | 128k | chat, code |
| `gpt-oss-120b` | complex | 128k | code, reasoning, chat, analysis |

---

## Groq

**Sign up:** https://console.groq.com

```bash
export GROQ_API_KEY=gsk_...
```

**Free tier:** llama-3.1-8b-instant (14,400/day), others (1,000/day).

**Default models:**

| Model | Tier | Context | Tasks |
|-------|------|---------|-------|
| `llama-3.1-8b-instant` | simple | 131k | chat, code |
| `llama-3.3-70b-versatile` | medium | 131k | code, chat, analysis |
| `llama-4-scout-17b-16e-instruct` | complex | 131k | code, reasoning, chat |
| `qwen/qwen3-32b` | reasoning | 131k | reasoning, code |

---

## Kilo

**Sign up:** https://kilo.ai

```bash
export KILO_API_KEY=kilo-...
```

**Free tier:** `kilo/auto-free` smart router (picks best available free model automatically).

**Default models:**

| Model | Tier | Context | Tasks |
|-------|------|---------|-------|
| `kilo/auto-free` | medium | 1M | code, reasoning, chat, analysis |
| `nvidia/nemotron-3-super-120b-a12b:free` | reasoning | 999k | reasoning, code, chat |
| `poolside/laguna-m.1:free` | complex | 262k | code, reasoning |

**Note:** `kilo/auto-free` is the recommended first choice — it automatically
selects the best free model on Kilo's network for each request.

---

## OpenRouter

**Sign up:** https://openrouter.ai/keys

```bash
export OPENROUTER_API_KEY=sk-or-v1-...
```

**Free tier:** models with `:free` suffix, typically 50–1000 req/day each.

**Default models:**

| Model | Tier | Context | Tasks |
|-------|------|---------|-------|
| `openai/gpt-oss-20b:free` | simple | 128k | chat, code, reasoning |
| `meta-llama/llama-3.3-70b-instruct:free` | medium | 131k | code, chat, analysis |
| `google/gemma-4-31b-it:free` | medium | 262k | chat, analysis, reasoning |
| `qwen/qwen3-coder:free` | complex | 1M | code, reasoning, analysis |
| `openai/gpt-oss-120b:free` | complex | 128k | code, reasoning, chat, analysis |
| `nousresearch/hermes-3-llama-3.1-405b:free` | complex | 128k | code, reasoning, chat, analysis |
| `deepseek/deepseek-v4-flash:free` | reasoning | 1M | code, reasoning, chat |
| `moonshotai/kimi-k2.6:free` | reasoning | 262k | reasoning, code, chat |

---

## BUW

**Sign up:** https://api.buw.xyz

```bash
export BUW_API_KEY=buw-...
```

**Base URL:** `https://api.buw.xyz/v1`

BUW provides virtual model endpoints that proxy to multiple backend providers.

**Default models:**

| Model | Tier | Context | Tasks |
|-------|------|---------|-------|
| `buw/omrp-auto` | medium | 1M | code, reasoning, chat, analysis |
| `buw/auto-kilo` | reasoning | 1M | code, reasoning, chat |

- `buw/omrp-auto` — OMRP-compatible auto-routing virtual model
- `buw/auto-kilo` — Kilo-aware auto-routing virtual model

---

## Adding Custom Models

Edit `~/.config/omrp/config.toml` (run `omrp init` to create it):

```toml
[[model]]
id       = "openrouter/claude-3-5-sonnet"
provider = "openrouter"
tasks    = ["code", "reasoning", "chat", "analysis"]
tool_use = true
ctx      = 200_000
tier     = "complex"

[[model]]
id       = "my-custom/model"
provider = "openrouter"         # or kilo, cerebras, groq, buw
tasks    = ["code", "chat"]
ctx      = 32_768
tier     = "medium"             # simple | medium | complex | reasoning
```

**Tier guide:**
- `simple` — fast, small models for easy tasks
- `medium` — balanced general-purpose models
- `complex` — large models for difficult tasks
- `reasoning` — specialized reasoning/thinking models

---

## Provider Priority

When the config defines multiple models for the same tier, OMRP uses the
Thompson Sampling (`select_thompson`) or BKG-FMR (`select`) scoring engine
to pick the best one based on latency, success history, and current load.

Provider keys in the **database take priority** over environment variables.
If a database key exists for a provider, the env var is ignored.

---

## Troubleshooting

### "auth error — check API key for X"

- The env var or DB key for provider `X` is set but invalid
- Check the key value; generate a new one from the provider's dashboard

### "No models available"

- No models are configured that pass the health filter
- Check `omrp status` to see model health scores
- Run `omrp init` to write the default config with built-in free models

### Model always returns errors

- The model may be temporarily unavailable or rate-limited
- OMRP will automatically fall back to the next model in the fallback chain
- If a model fails enough times, it's marked garbage and excluded automatically
- Recovery happens as soon as a request succeeds again
