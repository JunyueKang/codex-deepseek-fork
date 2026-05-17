# Codex 第三方 Provider 兼容版

基于 OpenAI Codex v0.130.0 的定制 fork，恢复并扩展了对第三方 OpenAI 兼容 API（如 DeepSeek）的完整支持。

## 为什么要改

OpenAI Codex 在近期版本中**官方移除了 Chat Completions 模式**——所有 `wire_api = "chat"` 的配置都会自动回退到 `/v1/responses` 协议，Codex 内部的 Chat 模式相关代码也已被移除。这导致 DeepSeek、Ollama、UniAPI 等只支持 `/v1/chat/completions` 的第三方 provider 无法使用。

本项目做了三件事：

1. **恢复 Chat 模式** — `wire_api = "chat"` 重新可用，内部自动将 Responses 格式请求适配为 Chat Completions API 调用
2. **支持 `--api_config=FILE`** — 启动时指定外部 TOML 配置文件，不同任务可切换不同的 API/模型（含 `/resume` 恢复会话时的配置保持）
3. **支持 `metadata = "local"`** — 在配置文件中直接写模型参数（context_window 等），不再依赖 provider 的 `/v1/models` 端点

## 主要改动

| 改动 | 说明 |
|---|---|
| `WireApi::Chat` 恢复 | `model-provider-info/src/lib.rs` — `wire_api = "chat"` 重新可用，`responses = "chat"` 别名兼容 |
| `reasoning_content` 字段 | `protocol/src/models.rs` + 15 个构造点 — Chat provider 返回的推理内容可存入历史并在多轮回放 |
| Chat 适配层 | `codex-api/src/endpoint/chat_completions.rs`（新）— Responses 请求 → Chat JSON；`codex-api/src/sse/chat_completions.rs`（新）— Chat SSE → ResponseEvent |
| `--api_config=FILE` | `utils/cli/src/config_override.rs` → `cli/src/main.rs` → `config/src/loader/mod.rs` → `tui/src/lib.rs`；以及 `tui/src/app_server_session.rs` + `app.rs` + `config_persistence.rs`（确保 `/resume` 恢复会话时不丢失配置） |
| `metadata = "local"` | `model-provider-info/src/lib.rs` — `ModelMetadataSource` 枚举 + `LocalModelInfo`；`model-provider/src/provider.rs` — 本地目录跳过 `/v1/models` |

详细 diff 见 [`resume-provider-fix-diff.md`](resume-provider-fix-diff.md)。

## 编译

```bash
cd codex-rust/codex-rs

# Debug 版
cargo build -p codex-cli

# Release 版（体积小、运行快）
cargo build -p codex-cli --release
```

> **注意**：入口是 `codex`（`cli` crate），**不是** `codex-tui`。`codex-tui` 二进制缺少 `--api_config` 的完整处理链路。

二进制位置：
- Debug: `target/debug/codex`
- Release: `target/release/codex`

## 使用

```bash
# 使用默认配置（$CODEX_HOME/config.toml）
./target/debug/codex

# 使用外部配置文件
./target/debug/codex --api_config=/Users/xxx/my_api_config.toml

# Release 版
./target/release/codex --api_config=/path/to/config.toml
```



## 配置文件详解

### 最小配置（DeepSeek Chat 模式）

```toml
model = "deepseek-v4-flash"
model_provider = "dsapi"
model_reasoning_effort = "medium"

[model_providers.dsapi]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
env_key = "dsoff_KEY"          # 环境变量名，API key 从该变量读取
wire_api = "chat"              # ★ 我们恢复的功能：走 /v1/chat/completions
```

### 完整配置（含 metadata = "local"）

```toml
model = "deepseek-v4-flash"
model_provider = "dsapi"
model_reasoning_effort = "medium"

[model_providers.dsapi]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
env_key = "dsoff_KEY"
wire_api = "chat"
metadata = "local"             # ★ 新增：不走 /v1/models 端点，用本地元数据
query_params = {}
request_max_retries = 4
stream_max_retries = 10

# ★ 新增：本地模型元数据
# 参数来源：https://api-docs.deepseek.com/quick_start/pricing
[[model_providers.dsapi.models]]
slug = "deepseek-v4-flash"
display_name = "DeepSeek V4 Flash"
context_window = 1000000        # 1M tokens
truncation_policy = { mode = "bytes", limit = 900000 }

[[model_providers.dsapi.models]]
slug = "deepseek-v4-pro"
display_name = "DeepSeek V4 Pro"
context_window = 1000000
truncation_policy = { mode = "bytes", limit = 900000 }

[features]
goals = true

# 信任的项目目录
[projects."/Users/xxx/work/my_project"]
trust_level = "trusted"
```

### 多 API 切换场景

在不同终端或任务中使用不同的 provider：

```bash
# 任务 A：使用 DeepSeek
./target/debug/codex --api_config=~/.codex/config_deepseek.toml

# 任务 B：使用另一个provider或另一个模型
./target/debug/codex --api_config=~/.codex/config_02.toml
```

### 关键字段说明

| 字段 | 说明 |
|---|---|
| `wire_api` | **我们恢复的**。官方近期版本已移除 `chat`（相关代码已删除），填 `"chat"` 会自动回退到 `responses`。本 fork 重新实现了 Chat 模式，支持 `"chat"` 和 `"responses"` |
| `responses` | `wire_api` 的别名（兼容旧配置写法 `responses = "chat"`） |
| `metadata` | **新增**。`"remote"`（默认）→ 从 provider 的 `GET /v1/models` 端点拉取模型元数据；`"local"` → 不调 `/v1/models`，从下方 `[[models]]` 本地读取。第三方 provider（如 DeepSeek）应选 `"local"`，否则会因 `/v1/models` 返回格式不兼容而触发 "Model metadata not found" 警告并降级到 fallback metadata |
| `[[models]]` | **新增**。仅在 `metadata = "local"` 时生效。只需填核心字段（slug、context_window 等），其余自动补齐默认值。详见下方字段表 |

| `env_key` | 存放 API key 的**环境变量名**（不是 key 本身），启动前需 `export dsoff_KEY=sk-xxx` |

### `[[models]]` 可用字段

| 字段 | 必填 | 默认值 | 说明 |
|---|---|---|---|
| `slug` | ✅ | — | 模型标识符，如 `"deepseek-v4-flash"` |
| `display_name` | ❌ | slug | UI 中显示的名称 |
| `context_window` | ❌ | `None` | 上下文窗口大小（tokens） |
| `truncation_policy` | ❌ | `{ mode = "bytes", limit = context_window 或 128000 }` | 截断策略 |
| `supported_reasoning_levels` | ❌ | low / medium / high | 支持的推理级别 |
| `supports_parallel_tool_calls` | ❌ | `true` | 是否支持并行工具调用 |
| `supports_reasoning_summaries` | ❌ | `false` | 是否输出推理摘要 |

不填的字段在转为内部 `ModelInfo` 时自动补齐合理默认值，不会出现「Model metadata not found」警告。

## 关于 Chat 模式和 DeepSeek 兼容

官方 Codex 内部使用 Responses API 格式。本项目在 `codex-api` 层加了一层适配：

```
用户请求（Responses 格式）
  → ChatCompletionsClient::from_responses()    # 转为 Chat JSON
  → POST /v1/chat/completions
  → SSE 流式响应
  → spawn_chat_completions_stream()            # 转回 ResponseEvent
  → TUI / Session 处理（无感）
```

Chat 模式下 `reasoning_content`（DeepSeek 的思考过程）也会被保存并在多轮对话中回传，不会丢失。

### 如果出现 "Model metadata for xxx not found" 警告

说明 provider 的 `/v1/models` 端点返回格式与 Codex 不兼容。**解决方法**：在 provider 配置中加 `metadata = "local"` 并填写 `[[models]]`（见上方完整配置示例），警告即消失。

## 其他

- 完整改动 diff 见 [`resume-provider-fix-diff.md`](resume-provider-fix-diff.md)
- 基础来自 [OpenAI Codex v0.130.0](https://github.com/openai/codex)
