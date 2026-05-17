# Codex DeepSeek Fork

> Custom fork of [OpenAI Codex](https://github.com/openai/codex) v0.130.0 with restored Chat Completions support for third-party providers (DeepSeek, Ollama, etc.).

[中文版](README_zh.md)

## Why

Recent versions of Codex **removed Chat Completions mode** — `wire_api = "chat"` configs silently fall back to `/v1/responses`, and all internal Chat-mode code was deleted. This breaks any provider that only supports `/v1/chat/completions` (DeepSeek, Ollama, OpenRouter, etc.).

This fork does three things:

1. **Restores Chat mode** — `wire_api = "chat"` works again, with Responses→Chat request adaptation and Chat SSE→ResponseEvent streaming
2. **Adds `--api_config=FILE`** — point to an external TOML config at startup; switch APIs/models per task (includes `/resume` config persistence)
3. **Adds `metadata = "local"`** — define model params (context_window, etc.) directly in config, no dependency on `/v1/models` endpoint

## Build

```bash
cd codex-rust/codex-rs
cargo build -p codex-cli --release
```

Binary: `target/release/codex` (**not** `codex-tui`).

## Quick Start

```bash
export MY_KEY=sk-xxx
./target/release/codex --api_config=/path/to/config.toml
```

## Config Example

```toml
model = "deepseek-v4-flash"
model_provider = "dsapi"
model_reasoning_effort = "medium"

[model_providers.dsapi]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
env_key = "MY_KEY"
wire_api = "chat"           # ★ Restored: uses /v1/chat/completions
metadata = "local"          # ★ New: skip /v1/models, use local catalog
query_params = {}
request_max_retries = 4
stream_max_retries = 10

[[model_providers.dsapi.models]]
slug = "deepseek-v4-flash"
display_name = "DeepSeek V4 Flash"
context_window = 1000000
truncation_policy = { mode = "bytes", limit = 900000 }

[[model_providers.dsapi.models]]
slug = "deepseek-v4-pro"
display_name = "DeepSeek V4 Pro"
context_window = 1000000
truncation_policy = { mode = "bytes", limit = 900000 }

[features]
goals = true
```

## Key Fields

| Field | Description |
|---|---|
| `wire_api` | **Restored**. Official Codex removed `"chat"`. This fork supports both `"chat"` and `"responses"` |
| `responses` | Alias for `wire_api` (legacy configs like `responses = "chat"` still work) |
| `metadata` | **New**. `"remote"` (default) → fetch from `/v1/models`. `"local"` → read from `[[models]]` below. Use `"local"` for third-party providers |
| `[[models]]` | **New**. Only active when `metadata = "local"`. Core fields only; the rest get sensible defaults |
| `env_key` | Environment variable name that holds the API key |

### `[[models]]` Fields

| Field | Required | Default | Notes |
|---|---|---|---|
| `slug` | ✅ | — | Model ID, e.g. `"deepseek-v4-flash"` |
| `display_name` | ❌ | `slug` | UI label |
| `context_window` | ❌ | `None` | Token limit |
| `truncation_policy` | ❌ | `{ mode = "bytes", limit = context_window or 128000 }` | |
| `supported_reasoning_levels` | ❌ | low / medium / high | |
| `supports_parallel_tool_calls` | ❌ | `true` | |
| `supports_reasoning_summaries` | ❌ | `false` | |

## Multi-API Workflow

```bash
# Task A: DeepSeek
./target/release/codex --api_config=~/config_deepseek.toml

# Task B: another provider
./target/release/codex --api_config=~/config_other.toml
```

## "Model metadata not found" Warning

Third-party `/v1/models` endpoints return a different JSON schema than Codex expects. **Fix**: set `metadata = "local"` and define `[[models]]` in your config (see example above).

## Full Diff

See [`resume-provider-fix-diff.md`](resume-provider-fix-diff.md) for line-by-line changes.

## License

Based on [OpenAI Codex](https://github.com/openai/codex). See upstream for license terms.
