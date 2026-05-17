# 自定义 Provider、Chat 模式与 `/resume` 修复 Diff 说明

本文档基于以下对比命令整理：

```bash
diff -u /Users/a0000/work/codex-rust-original/codex-rs/<file> \
        /Users/a0000/work/codex-rust/codex-rs/<file>
```

说明：当前工作区相对 `v0.130.0-original` 还有其它历史改动。本文只记录和以下三条链路直接相关的改动：

- 支持 `wire_api = "chat"`，把内部 Responses 风格请求适配到 OpenAI 兼容的 Chat Completions API。
- 支持 `--api_config=FILE`，用外部 TOML 文件作为用户模型/provider 配置。
- 修复 `/resume` 后 provider/config 链路丢失，确保恢复会话走历史会话原本的模型与 provider。

如果目标是“从官方 `v0.130.0` 手工改到当前可用功能”，请按本文的实施顺序操作，而不是只挑某个文件改。尤其是 Chat 模式会改动 `ResponseItem` 枚举字段，漏掉任意构造点都会导致编译失败。

## 从官方版本迁移的实施顺序

推荐顺序如下：

1. 先改协议和 provider 配置层：
   - `model-provider-info/src/lib.rs`
   - `model-provider-info/src/model_provider_info_tests.rs`
   - `protocol/src/models.rs`
   - `protocol/src/items.rs`
   - `protocol/src/protocol.rs`
2. 补齐 `reasoning_content` 新字段引发的所有构造点：
   - `core/src/codex_thread.rs`
   - `core/src/compact.rs`
   - `core/src/context/fragment.rs`
   - `core/src/context_manager/updates.rs`
   - `core/src/goals.rs`
   - `core/src/session/handlers.rs`
   - `core/src/session/mod.rs`
   - `core/src/tasks/mod.rs`
   - `core/src/tasks/review.rs`
   - `core/src/tasks/user_shell.rs`
   - `core/tests/common/responses.rs`
   - `codex-api/tests/clients.rs`
   - `external-agent-sessions/src/export.rs`
   - `memories/write/src/phase1.rs`
   - `tui/src/app/side.rs`
3. 新增并接入 Chat Completions API：
   - `codex-api/src/endpoint/chat_completions.rs`
   - `codex-api/src/sse/chat_completions.rs`
   - `codex-api/src/endpoint/mod.rs`
   - `codex-api/src/sse/mod.rs`
   - `codex-api/src/lib.rs`
   - `core/src/client.rs`
   - `core/src/client_common.rs`
4. 让远端 thread config 认识 Chat：
   - `config/src/thread_config/proto/codex.thread_config.v1.proto`
   - `config/src/thread_config/proto/codex.thread_config.v1.rs`
   - `config/src/thread_config/remote.rs`
5. 接入 `--api_config=FILE`：
   - `utils/cli/src/config_override.rs`
   - `cli/src/main.rs`
   - `config/src/state.rs`
   - `config/src/loader/mod.rs`
   - `tui/src/lib.rs`
   - `app-server-test-client/src/lib.rs`
6. 修复 `/resume`：
   - `tui/src/app_server_session.rs`
   - `tui/src/app.rs`
   - `tui/src/app/config_persistence.rs`
   - `tui/src/app/test_support.rs`
   - `tui/src/app/tests.rs`

不需要为了这项功能手工修改 `Cargo.lock` 中大量 crate 的 `version = "0.130.0"`。当前 diff 里 `Cargo.lock` 的版本号变化是工作区元数据差异，不是 Chat、`--api_config` 或 `/resume` 修复的必要条件；照功能迁移时应避免把它当成必改代码。

## 必改点索引

### `reasoning_content` 字段的完整补齐清单

Chat provider 常见会在 delta 里返回 `reasoning_content`。为了让它能进入历史上下文并在下一轮回放，本文给 `ResponseInputItem::Message`、`ResponseItem::Message`、`ResponseItem::FunctionCall`、`ResponseItem::CustomToolCall` 增加了 `reasoning_content` 字段。

这会让所有直接构造这些 enum variant 的代码都必须补字段。规则如下：

- 普通用户/系统/开发者/测试消息：补 `reasoning_content: None`。
- 从旧 `ResponseItem::Message` 解构再重建的地方：必须把 `reasoning_content` 解出来并 clone/传回，不能写死 `None`，否则会丢历史 reasoning。
- Chat SSE parser 在最终 assistant message 或 function call 上使用 `non_empty_reasoning_content(state)`。
- Chat request adapter 在把历史 tool call group 变回 Chat messages 时，要收集 tool call 上的 `reasoning_content` 并写到 assistant tool_calls message。

官方版本手工迁移时，下面这些文件中的直接构造点必须处理：

```text
protocol/src/models.rs
protocol/src/items.rs
protocol/src/protocol.rs
core/src/codex_thread.rs
core/src/compact.rs
core/src/context/fragment.rs
core/src/context_manager/updates.rs
core/src/goals.rs
core/src/session/handlers.rs
core/src/session/mod.rs
core/src/tasks/mod.rs
core/src/tasks/review.rs
core/src/tasks/user_shell.rs
core/tests/common/responses.rs
codex-api/tests/clients.rs
external-agent-sessions/src/export.rs
memories/write/src/phase1.rs
tui/src/app/side.rs
```

可以用下面命令检查是否还有漏掉的构造点：

```bash
rg -n "ResponseItem::Message \\{|ResponseInputItem::Message \\{|ResponseItem::FunctionCall \\{|ResponseItem::CustomToolCall \\{" codex-rs -g '*.rs'
```

如果编译报类似下面的错误，说明这一节漏改：

```text
missing field `reasoning_content` in initializer of `ResponseItem::Message`
missing field `reasoning_content` in initializer of `ResponseInputItem::Message`
missing field `reasoning_content` in initializer of `ResponseItem::FunctionCall`
missing field `reasoning_content` in initializer of `ResponseItem::CustomToolCall`
pattern does not mention field `reasoning_content`
```

### `CliConfigOverrides` 字段扩散清单

`CliConfigOverrides` 新增 `api_config: Option<PathBuf>` 后，所有手动构造它的地方都要填字段。真实功能入口在 `cli/src/main.rs`，其它内部构造点应填 `api_config: None`。

必须处理：

```text
utils/cli/src/config_override.rs
cli/src/main.rs
tui/src/lib.rs
app-server-test-client/src/lib.rs
```

检查命令：

```bash
rg -n "CliConfigOverrides \\{" codex-rs -g '*.rs'
```

如果漏改，会出现：

```text
missing field `api_config` in initializer of `CliConfigOverrides`
```

### `LoaderOverrides` 字段扩散清单

`LoaderOverrides` 新增 `user_config_path` 后，默认构造路径必须保持 `None`，CLI 路径才填 `Some(path)`。

必须处理：

```text
config/src/state.rs
config/src/loader/mod.rs
cli/src/main.rs
tui/src/lib.rs
tui/src/app.rs
tui/src/app/config_persistence.rs
tui/src/app/test_support.rs
tui/src/app/tests.rs
```

如果只改了 CLI 和 loader，没有把 `LoaderOverrides` 存进 `App` 并传给 `rebuild_config_for_cwd()`，会出现运行时 bug：刚启动直接对话正常，但 `/resume` 后报 `Model provider 'uniapi' not found`。

### Chat 模块接入清单

新增文件以后必须接入模块树，否则 core 无法引用：

```text
codex-api/src/endpoint/chat_completions.rs
codex-api/src/sse/chat_completions.rs
codex-api/src/endpoint/mod.rs
codex-api/src/sse/mod.rs
codex-api/src/lib.rs
```

如果漏掉 re-export，会出现：

```text
unresolved import `codex_api::ChatCompletionsClient`
unresolved import `codex_api::ChatCompletionsOptions`
cannot find function `spawn_chat_completions_stream`
```

其中两个新增文件建议从当前工作区整文件复制，不建议只按文档片段手敲：

```text
codex-api/src/endpoint/chat_completions.rs  # 458 行
codex-api/src/sse/chat_completions.rs       # 474 行
```

原因是这两个文件包含大量容易漏掉的私有 helper：

- `chat_messages_from_response_items`
- `collect_complete_tool_call_group`
- `chat_message_content_from_content_items`
- `chat_tools_from_responses_tools`
- `process_chat_completions_sse`
- `handle_chat_delta`
- `ensure_assistant_item_started`
- `finish_chat_stream`
- `non_empty_reasoning_content`

漏掉任意 helper 都可能是编译失败；更隐蔽的是漏掉 `collect_complete_tool_call_group` 的完整输出检查，会导致历史工具调用序列不完整时 provider 返回 400。

### `WireApi::Chat` 映射清单

`WireApi::Chat` 要同时存在于 provider TOML、core 分发、远端 thread config proto 映射三处。

必须处理：

```text
model-provider-info/src/lib.rs
model-provider-info/src/model_provider_info_tests.rs
core/src/client.rs
config/src/thread_config/proto/codex.thread_config.v1.proto
config/src/thread_config/proto/codex.thread_config.v1.rs
config/src/thread_config/remote.rs
```

如果只让 TOML 能解析 `chat`，但 core 不分发到 Chat API，请求仍会走 `/responses`。如果 core 分发了但 websocket 没禁用，Chat provider 仍可能走 `wss://.../responses`。

## 验证顺序

从官方版本照文档改完后，建议按下面顺序验证：

```bash
cd /Users/a0000/work/codex-rust/codex-rs
just fmt
cargo test -p codex-model-provider-info test_deserialize_chat_wire_api
cargo test -p codex-api streamed_text_uses_same_message_item_for_start_and_done
cargo test -p codex-tui app_server_session::tests::thread_resume_params_do_not_override_model_or_provider_for_embedded_sessions
cargo test -p codex-tui app::config_persistence::tests::rebuild_config_for_cwd_preserves_loader_overrides
```

如果想先快速发现全局编译漏项，可以运行：

```bash
cargo check -p codex-core -p codex-api -p codex-tui -p codex-cli
```

常见失败对应关系：

- `missing field reasoning_content`：回到“`reasoning_content` 字段的完整补齐清单”。
- `missing field api_config`：回到“`CliConfigOverrides` 字段扩散清单”。
- `no variant or associated item named Chat`：回到“`WireApi::Chat` 映射清单”。
- `unresolved import ChatCompletionsClient`：回到“Chat 模块接入清单”。
- 运行时仍请求 `wss://api.openai.com/v1/responses`：检查 `responses_websocket_enabled()` 是否对 `WireApi::Chat` 返回 false，以及 `stream()` 分发是否有 `WireApi::Chat => stream_chat_completions_api(...)`。
- `/resume` 报 `Model provider 'uniapi' not found`：检查 `App` 是否保存了 `loader_overrides`，以及 `rebuild_config_for_cwd()` 是否调用 `.loader_overrides(self.loader_overrides.clone())`。

## 问题背景

现象分两段：

1. `/resume` 后把历史会话里的 `deepseek-v4-pro` 放到当前默认 OpenAI provider 上请求，导致走到 `wss://api.openai.com/v1/responses`，并出现缺少认证的 401。
2. 修正 provider 覆盖后，恢复流程读到了历史 provider `uniapi`，但运行期重建配置时没有复用启动时的 loader 配置层，导致报 `Model provider 'uniapi' not found`。

最终修复目标：

- `/resume` 时不要用当前 TUI 配置覆盖历史会话的 `model` / `model_provider`。
- 运行期重建配置时复用启动时的 `LoaderOverrides`，确保 loader/managed 配置层里定义的 provider 仍然可见。

## A. `/resume` Provider 修复

### 1. `tui/src/app_server_session.rs`

#### 改动点

`thread_resume_params_from_config()` 不再向 `thread/resume` 请求发送当前配置里的 `model` 和 `model_provider`。

#### 代码 diff

```diff
 ThreadResumeParams {
     thread_id: thread_id.to_string(),
-    model: config.model.clone(),
-    model_provider: thread_params_mode.model_provider_from_config(&config),
+    model: None,
+    model_provider: None,
     cwd: thread_cwd_from_config(&config, thread_params_mode, remote_cwd_override),
     approval_policy: Some(config.permissions.approval_policy.value().into()),
     approvals_reviewer: approvals_reviewer_override_from_config(&config),
```

#### 作用

此前 `/resume` 会把“当前启动 TUI 时加载出的 model/provider”作为显式覆盖传给 app-server。这样恢复旧会话时，历史会话的 provider 可能被当前默认 provider 覆盖，造成：

- DeepSeek/UniAPI 会话被错误地套到 OpenAI provider 上。
- 后续请求走 OpenAI websocket endpoint。
- 自定义 provider 的鉴权和 base_url 都丢失。

改成 `None` 后，app-server 可以按持久化的 thread metadata 恢复历史会话原本的 `model` 和 `model_provider`。

#### 新增测试

```rust
#[tokio::test]
async fn thread_resume_params_do_not_override_model_or_provider_for_embedded_sessions() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = build_config(&temp_dir).await;
    let thread_id = ThreadId::new();

    let params = thread_resume_params_from_config(
        config,
        thread_id,
        ThreadParamsMode::Embedded,
        /*remote_cwd_override*/ None,
    );

    assert_eq!(params.model, None);
    assert_eq!(params.model_provider, None);
}
```

测试保证 embedded TUI 的 `/resume` 不再发送模型/provider 覆盖。

### 2. `tui/src/app.rs`

#### 改动点

`App` 保存启动时传入的 `LoaderOverrides`，并让 `App::run()` 接收该参数。

#### 代码 diff

```diff
 use codex_config::ConfigLayerStackOrdering;
+use codex_config::LoaderOverrides;
 use codex_config::types::ApprovalsReviewer;
```

```diff
 pub(crate) active_profile: Option<String>,
 cli_kv_overrides: Vec<(String, TomlValue)>,
+loader_overrides: LoaderOverrides,
 harness_overrides: ConfigOverrides,
```

```diff
 pub async fn run(
     tui: &mut tui::Tui,
     mut app_server: AppServerSession,
     mut config: Config,
     cli_kv_overrides: Vec<(String, TomlValue)>,
+    loader_overrides: LoaderOverrides,
     harness_overrides: ConfigOverrides,
```

```diff
 active_profile,
 cli_kv_overrides,
+loader_overrides,
 harness_overrides,
```

#### 作用

TUI 启动时会用 `LoaderOverrides` 加载配置。某些 provider 定义可能来自这些 loader 配置层，例如 managed config、测试/外部注入配置等。

此前 `App` 只保存了：

- `cli_kv_overrides`
- `harness_overrides`

但没有保存 `loader_overrides`。因此启动后如果执行 `/resume`，运行期重新 build config 时会少加载一层配置，可能找不到历史会话里的 provider，例如 `uniapi`。

新增字段后，运行期配置重建可以继续使用同一套 loader 配置来源。

### 3. `tui/src/app/config_persistence.rs`

#### 改动点

`rebuild_config_for_cwd()` 调用 `ConfigBuilder` 时传入 `self.loader_overrides.clone()`。

#### 代码 diff

```diff
 ConfigBuilder::default()
     .codex_home(self.config.codex_home.to_path_buf())
     .cli_overrides(self.cli_kv_overrides.clone())
+    .loader_overrides(self.loader_overrides.clone())
     .harness_overrides(overrides)
     .build()
```

#### 作用

`rebuild_config_for_cwd()` 是 `/resume`、切换 cwd、刷新内存配置等运行期路径会调用的配置重建函数。

这次修复确保它和启动路径一致：

- 保留 CLI `-c` 覆盖。
- 保留 harness 覆盖。
- 同时保留 loader/managed 配置层。

这样恢复会话时，app-server 读取到历史 provider `uniapi` 后，重新加载配置也能找到 `model_providers.uniapi`。

#### 新增测试

```rust
#[tokio::test]
async fn rebuild_config_for_cwd_preserves_loader_overrides() -> Result<()> {
    let mut app = make_test_app().await;
    let codex_home = tempdir()?;
    let managed_config_path = codex_home.path().join("managed_config.toml");
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.loader_overrides =
        LoaderOverrides::with_managed_config_path_for_tests(managed_config_path.clone());
    std::fs::write(
        &managed_config_path,
        r#"
model = "managed-model"
model_provider = "managed-provider"

[model_providers.managed-provider]
name = "managed"
base_url = "http://localhost:1234/v1"
wire_api = "responses"
"#,
    )?;

    let config = app
        .rebuild_config_for_cwd(app.config.cwd.to_path_buf())
        .await?;

    assert_eq!(config.model, Some("managed-model".to_string()));
    assert_eq!(config.model_provider_id, "managed-provider");
    assert_eq!(
        config.model_provider.base_url.as_deref(),
        Some("http://localhost:1234/v1")
    );
    Ok(())
}
```

测试模拟 provider 只存在于 loader/managed 配置层的情况。没有这次修复时，重建配置会找不到 `managed-provider`。

### 4. `tui/src/lib.rs`

#### 改动点

启动路径中的配置加载函数改为带 loader overrides 的版本，并把 `loader_overrides` 继续传给 `App::run()`。

#### 代码 diff

```diff
-use crate::legacy_core::config::load_config_as_toml_with_cli_overrides;
+use crate::legacy_core::config::load_config_as_toml_with_cli_and_loader_overrides;
```

```diff
-let config_toml = match load_config_as_toml_with_cli_overrides(
+let config_toml = match load_config_as_toml_with_cli_and_loader_overrides(
     &codex_home,
     config_cwd.as_ref(),
     cli_kv_overrides.clone(),
+    loader_overrides.clone(),
 )
```

```diff
 let mut config = load_config_or_exit(
     cli_kv_overrides.clone(),
     overrides.clone(),
+    loader_overrides.clone(),
     cloud_requirements.clone(),
 )
```

```diff
 let app_result = App::run(
     &mut tui,
     app_server,
     config,
     cli_kv_overrides.clone(),
+    loader_overrides,
     overrides.clone(),
```

```diff
 async fn load_config_or_exit(
     cli_kv_overrides: Vec<(String, toml::Value)>,
     overrides: ConfigOverrides,
+    loader_overrides: LoaderOverrides,
     cloud_requirements: CloudRequirementsLoader,
 ) -> Config {
```

```diff
 ConfigBuilder::default()
     .cli_overrides(cli_kv_overrides)
     .harness_overrides(overrides)
+    .loader_overrides(loader_overrides)
     .cloud_requirements(cloud_requirements)
```

#### 作用

这保证启动时读取 config.toml、迁移后 reload config、trust/onboarding 后 reload config、fallback cwd reload config 等路径都用同一套 loader 配置。

它和 `App` 保存 `loader_overrides` 是一组修复：

- `lib.rs` 负责把 loader 配置从启动入口一路传进 app。
- `app.rs` 负责保存该配置。
- `config_persistence.rs` 负责在运行期重建配置时继续使用它。

### 5. `tui/src/app/test_support.rs` 和 `tui/src/app/tests.rs`

#### 改动点

测试用的 `App { ... }` 初始化补充默认 `loader_overrides` 字段。

#### 代码 diff

```diff
 active_profile: None,
 cli_kv_overrides: Vec::new(),
+loader_overrides: LoaderOverrides::default(),
 harness_overrides: ConfigOverrides::default(),
```

#### 作用

`App` 新增字段后，所有测试构造器都需要提供默认值。这里不改变测试行为，只让测试 App 使用默认 loader 配置。

## B. Chat Completions 兼容模式

这组改动恢复并实现 `wire_api = "chat"`，用于 DeepSeek、UniAPI、Ollama 兼容端、OpenAI-compatible 聚合网关等只支持 `/v1/chat/completions` 的 provider。

核心设计是：core 仍然构造 Codex 内部统一的 `ResponsesApiRequest`，新增 `codex-api` 适配层在真正发请求前把它转换成 Chat Completions JSON；收到 Chat SSE 后再转换回内部 `ResponseEvent`。这样 TUI、session、工具调用处理逻辑不需要整体改成另一套协议。

### 1. `model-provider-info/src/lib.rs`

#### 改动点

`WireApi` 重新支持 `Chat`，并允许 TOML 中写 `wire_api = "chat"`。同时给 `wire_api` 字段增加 `alias = "responses"`，兼容旧配置里用 `responses = "chat"` 的写法。

#### 代码 diff

```diff
 pub enum WireApi {
     /// The Responses API exposed by OpenAI at `/v1/responses`.
     #[default]
     Responses,
+    /// The Chat Completions API exposed by OpenAI-compatible providers at `/v1/chat/completions`.
+    Chat,
 }
```

```diff
 match value.as_str() {
     "responses" => Ok(Self::Responses),
-    "chat" => Err(serde::de::Error::custom(CHAT_WIRE_API_REMOVED_ERROR)),
-    _ => Err(serde::de::Error::unknown_variant(&value, &["responses"])),
+    "chat" => Ok(Self::Chat),
+    _ => Err(serde::de::Error::unknown_variant(
+        &value,
+        &["responses", "chat"],
+    )),
 }
```

```diff
-#[serde(default)]
+#[serde(default, alias = "responses")]
 pub wire_api: WireApi,
```

#### 作用和细节

- `wire_api = "chat"` 不再报 “no longer supported”，而是进入 Chat Completions 分支。
- `Display` 中同步增加 `"chat"`，否则日志、trace、配置序列化会显示不完整。
- `alias = "responses"` 不是把 Chat 当 Responses，而是兼容字段名：`responses = "chat"` 会被 serde 读到 `wire_api` 字段里。
- 测试 `test_deserialize_chat_wire_api` 和 `test_deserialize_responses_alias_as_wire_api` 覆盖了这两种配置写法。

### 2. `core/src/client.rs`

#### 改动点

新增 Chat Completions endpoint 常量、options builder、streaming 方法，并在模型请求分发时根据 provider 的 `wire_api` 选择 Chat 或 Responses。

#### 重点代码

```rust
const RESPONSES_ENDPOINT: &str = "/responses";
const CHAT_COMPLETIONS_ENDPOINT: &str = "/chat/completions";
```

```rust
pub fn responses_websocket_enabled(&self) -> bool {
    if self.state.provider.info().wire_api == WireApi::Chat
        || !self.state.provider.info().supports_websockets
        || self.state.disable_websockets.load(Ordering::Relaxed)
        || (*CODEX_RS_SSE_FIXTURE).is_some()
    {
        return false;
    }
    true
}
```

```rust
fn build_chat_completions_options(
    &self,
    turn_metadata_header: Option<&str>,
) -> ApiChatCompletionsOptions {
    let turn_metadata_header = parse_turn_metadata_header(turn_metadata_header);
    let session_id = self.client.state.session_id.to_string();
    let thread_id = self.client.state.thread_id.to_string();
    ApiChatCompletionsOptions {
        session_id: Some(session_id),
        thread_id: Some(thread_id),
        session_source: Some(self.client.state.session_source.clone()),
        extra_headers: {
            let mut headers = build_responses_headers(
                self.client.state.beta_features_header.as_deref(),
                Some(&self.turn_state),
                turn_metadata_header.as_ref(),
            );
            headers.extend(self.client.build_responses_identity_headers());
            headers
        },
        turn_state: Some(Arc::clone(&self.turn_state)),
    }
}
```

```rust
match self.client.state.provider.info().wire_api {
    WireApi::Responses => {
        self.stream_responses_api(
            prompt,
            model_info,
            session_telemetry,
            effort,
            summary,
            service_tier,
            turn_metadata_header,
            inference_trace,
        )
        .await
    }
    WireApi::Chat => {
        self.stream_chat_completions_api(
            prompt,
            model_info,
            session_telemetry,
            effort,
            summary,
            service_tier,
            turn_metadata_header,
            inference_trace,
        )
        .await
    }
}
```

#### 作用和细节

- Chat provider 必须禁用 Responses websocket。否则会再次走到 `wss://.../responses`，也就是最初 `/resume` 错误里的异常链路。
- Chat 分支仍调用 `build_responses_request()`，目的是复用现有 prompt、工具、reasoning、service tier、telemetry 等构造逻辑。
- Chat 请求的 telemetry route 使用 `CHAT_COMPLETIONS_ENDPOINT`，便于日志和错误定位时区分 `/responses` 与 `/chat/completions`。
- 401 处理、auth recovery、SSE fixture、inference trace 的处理方式和 Responses HTTP 分支保持一致，避免只在正常请求路径工作，鉴权刷新或 fixture 测试路径失效。
- `force_http_fallback()` 改为只有 websocket 原本启用时才返回 activated，避免 Chat provider 本来就没有 websocket 时误报发生了 fallback。

### 3. `codex-api/src/endpoint/chat_completions.rs`

#### 改动点

新增 `ChatCompletionsClient`，负责把 `ResponsesApiRequest` 转成 Chat Completions 请求并 POST 到 `chat/completions`。

#### 重点代码

```rust
#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    tool_choice: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    stream: bool,
    stream_options: ChatStreamOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<codex_protocol::openai_models::ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
}
```

```rust
impl ChatCompletionsRequest {
    fn from_responses(request: ResponsesApiRequest) -> Self {
        Self {
            model: request.model,
            messages: chat_messages_from_response_items(request.instructions, request.input),
            tools: chat_tools_from_responses_tools(request.tools),
            tool_choice: request.tool_choice,
            parallel_tool_calls: Some(request.parallel_tool_calls),
            stream: true,
            stream_options: ChatStreamOptions {
                include_usage: true,
            },
            reasoning_effort: request
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort),
            thinking: request
                .reasoning
                .as_ref()
                .map(|_| json!({"type": "enabled"})),
            service_tier: request.service_tier,
            prompt_cache_key: request.prompt_cache_key,
        }
    }
}
```

```rust
self.session
    .stream_with(
        Method::POST,
        "chat/completions",
        extra_headers,
        Some(body),
        |req| {
            req.headers.insert(
                http::header::ACCEPT,
                HeaderValue::from_static("text/event-stream"),
            );
        },
    )
    .await?;
```

#### 消息转换细节

```rust
fn chat_message_role(role: &str) -> &str {
    match role {
        "developer" => "system",
        other => other,
    }
}
```

- Chat Completions 没有 Responses API 的 `developer` role，这里映射为 `system`。
- 纯文本消息压成 `content: "..."`；包含图片时使用 `content: [{ type: "text" }, { type: "image_url" }]`。
- `ImageDetail::Original` 映射成 `"auto"`，因为 Chat Completions 常见协议没有 `original` detail。

#### 工具调用转换细节

```rust
fn collect_complete_tool_call_group(
    input: &[ResponseItem],
    start_idx: usize,
) -> (usize, Vec<ChatMessage>) {
    ...
    if outputs.len() != call_ids.len() {
        return (scan_idx, Vec::new());
    }
    ...
}
```

- 历史上下文里一组 assistant tool calls 只有在后面能找到完整 tool outputs 时，才会转成 Chat Completions 的 assistant `tool_calls` + tool messages。
- 如果工具输出不完整，这组工具调用不会被硬塞进 chat history，避免构造出 provider 无法接受的不完整消息序列。
- 只把 Responses 工具里的 `type = "function"` 转成 Chat `tools`，其它 Responses 专有工具类型会被过滤。

```rust
fn chat_tools_from_responses_tools(tools: Vec<Value>) -> Vec<Value> {
    tools
        .into_iter()
        .filter_map(|tool| match tool.get("type").and_then(Value::as_str) {
            Some("function") => Some(json!({
                "type": "function",
                "function": {
                    "name": tool.get("name").cloned().unwrap_or(Value::Null),
                    "description": tool.get("description").cloned().unwrap_or(Value::String(String::new())),
                    "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                }
            })),
            _ => None,
        })
        .collect()
}
```

### 4. `codex-api/src/sse/chat_completions.rs`

#### 改动点

新增 Chat SSE parser，把 Chat Completions 的流式 chunk 转成内部 `ResponseEvent`。

#### 重点代码

```rust
pub fn spawn_chat_completions_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    turn_state: Option<Arc<OnceLock<String>>>,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if let Some(turn_state) = turn_state.as_ref()
        && let Some(header_value) = stream_response
            .headers
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok())
    {
        let _ = turn_state.set(header_value.to_string());
    }
    ...
}
```

```rust
if sse.data.trim() == "[DONE]" {
    finish_chat_stream(&mut state, &tx_event).await;
    return;
}
```

```rust
if let Some(content) = delta.content
    && !content.is_empty()
{
    ensure_assistant_item_started(state, tx_event).await?;
    state.assistant_text.push_str(&content);
    if tx_event
        .send(Ok(ResponseEvent::OutputTextDelta(content)))
        .await
        .is_err()
    {
        return Err(());
    }
}
```

```rust
if let Some(tool_calls) = delta.tool_calls {
    for tool_call in tool_calls {
        let call = state.tool_calls.entry(tool_call.index).or_default();
        if let Some(id) = tool_call.id {
            call.id = Some(id);
        }
        if let Some(function) = tool_call.function {
            if let Some(name) = function.name {
                call.name = Some(name);
            }
            if let Some(arguments) = function.arguments
                && !arguments.is_empty()
            {
                call.arguments.push_str(&arguments);
                ...
                if tx_event
                    .send(Ok(ResponseEvent::ToolCallInputDelta {
                        item_id,
                        call_id: call.id.clone(),
                        delta: arguments,
                    }))
                    .await
                    .is_err()
                {
                    return Err(());
                }
            }
        }
    }
}
```

#### 作用和细节

- 一开始先发 `ResponseEvent::Created`，让上层状态机看到和 Responses SSE 类似的生命周期。
- Chat 的 `delta.content` 会先触发 `OutputItemAdded(Message)`，再持续发送 `OutputTextDelta`，最后在 `[DONE]` 时发送 `OutputItemDone(Message)`。
- Chat 的工具调用参数通常是分片到达的，代码按 `tool_call.index` 聚合 `id/name/arguments`，最后在 `[DONE]` 时发 `ResponseItem::FunctionCall`。
- `usage` 通过 `stream_options.include_usage = true` 要求 provider 返回，并映射成内部 `TokenUsage`：
  - `prompt_tokens` -> `input_tokens`
  - `completion_tokens` -> `output_tokens`
  - `prompt_tokens_details.cached_tokens` -> `cached_input_tokens`
  - `completion_tokens_details.reasoning_tokens` -> `reasoning_output_tokens`
- 支持 provider 返回的 `reasoning_content` delta，并在最终 `Message` 或 `FunctionCall` 上带回，避免 reasoning 丢失。
- 测试 `streamed_text_uses_same_message_item_for_start_and_done` 保证同一段文本流的 start/done 使用同一个 message item id，防止 TUI 中出现重复或错位消息。

### 5. `protocol/src/models.rs`、`items.rs`、`protocol.rs`

#### 改动点

给内部 `ResponseItem` 的消息和工具调用补充 `reasoning_content` 字段，并在构造默认消息的位置填 `None`。

#### 代码 diff

```diff
 ResponseItem::Message {
     ...
     phase: Option<MessagePhase>,
+    reasoning_content: Option<String>,
 },
```

```diff
 ResponseItem::FunctionCall {
     ...
     call_id: String,
+    reasoning_content: Option<String>,
 },
```

```diff
 ResponseItem::CustomToolCall {
     ...
     input: String,
+    reasoning_content: Option<String>,
 },
```

#### 作用和细节

Chat 兼容层需要保存和回放 provider 返回的 `reasoning_content`。如果只在 SSE parser 中接收但不进入 `ResponseItem`，下一轮构造 chat history 时 reasoning 会丢失，工具调用前后的 reasoning 也无法随历史上下文传回 provider。

### 6. `config/src/thread_config/remote.rs` 和 proto 生成文件

#### 改动点

远端 thread config 的 `WireApi` 映射增加 Chat。

#### 代码 diff

```diff
let wire_api = match proto::WireApi::try_from(provider.wire_api) {
    Ok(proto::WireApi::Responses) => WireApi::Responses,
+   Ok(proto::WireApi::Chat) => WireApi::Chat,
    Ok(proto::WireApi::Unspecified) => {
        return Err(parse_error("remote thread config omitted wire_api"));
    }
```

```diff
fn proto_wire_api(wire_api: WireApi) -> proto::WireApi {
    match wire_api {
        WireApi::Responses => proto::WireApi::Responses,
+       WireApi::Chat => proto::WireApi::Chat,
    }
}
```

#### 作用

远端/嵌入式恢复 thread config 时，provider 的 wire api 不会被强制丢成 Responses。否则 Chat provider 的历史会话在恢复后仍可能错误走 `/responses`。

### 7. `codex-api/src/endpoint/mod.rs`、`codex-api/src/sse/mod.rs`、`codex-api/src/lib.rs`

#### 改动点

把新增的 Chat endpoint 和 SSE parser 接入 `codex-api` 模块导出。

#### 代码 diff

```diff
+pub(crate) mod chat_completions;
 pub(crate) mod compact;
```

```diff
+pub use chat_completions::ChatCompletionsClient;
+pub use chat_completions::ChatCompletionsOptions;
 pub use compact::CompactClient;
```

```diff
+pub(crate) mod chat_completions;
 pub(crate) mod responses;
 
+pub use chat_completions::spawn_chat_completions_stream;
 pub use responses::spawn_response_stream;
```

#### 作用

`core/src/client.rs` 使用的是 `codex_api::ChatCompletionsClient` 和 `codex_api::ChatCompletionsOptions`。如果只新增文件但不导出，core 无法调用 Chat 分支；如果只导出 endpoint 但不导出 SSE parser，`ChatCompletionsClient::stream_request()` 无法把 HTTP stream 包装成内部 `ResponseStream`。

## C. `--api_config=FILE` 外部模型配置文件

这组改动让用户可以启动时指定一个 TOML 文件作为“用户配置文件”，用于放模型、provider、profile 等配置。它只替代默认的 `$CODEX_HOME/config.toml` 读取路径，不改变 auth、sessions、state 的 `$CODEX_HOME` 位置。

### 1. `utils/cli/src/config_override.rs`

#### 改动点

在全局 CLI config overrides 里新增 `--api_config`。

#### 代码 diff

```diff
use std::path::PathBuf;
```

```rust
/// Load user configuration from this TOML file instead of
/// `$CODEX_HOME/config.toml`. Auth, sessions, and other state still use
/// `$CODEX_HOME`.
#[arg(long = "api_config", value_name = "FILE", global = true)]
pub api_config: Option<PathBuf>,
```

#### 作用和细节

- 参数名是 `--api_config`，不是 `--api-config`。因为 clap 指定了 `long = "api_config"`。
- 字段挂在 `CliConfigOverrides` 上，并设置 `global = true`，所以 TUI 子命令路径也能拿到。
- 注释明确：只替换用户 config TOML，不替换 auth/session/state 目录。这一点对 `/resume` 很重要，历史 session 仍在原 `$CODEX_HOME/sessions` 下。

### 2. `cli/src/main.rs`

#### 改动点

把 CLI 参数转换成 `codex_config::LoaderOverrides`，并传给 TUI 启动函数。

#### 代码 diff

```rust
fn loader_overrides_from_cli(
    config_overrides: &CliConfigOverrides,
) -> codex_config::LoaderOverrides {
    codex_config::LoaderOverrides {
        user_config_path: config_overrides.api_config.clone(),
        ..Default::default()
    }
}
```

```diff
let exit_info = run_interactive_tui(
    interactive,
+   loader_overrides_from_cli(&root_config_overrides),
    root_remote.clone(),
    root_remote_auth_token_env.clone(),
    arg0_paths.clone(),
)
```

```diff
async fn run_interactive_tui(
    mut interactive: TuiCli,
+   loader_overrides: codex_config::LoaderOverrides,
    remote: Option<String>,
    remote_auth_token_env: Option<String>,
    arg0_paths: Arg0DispatchPaths,
)
```

```diff
codex_tui::run_main(
    interactive,
    arg0_paths,
-   codex_config::LoaderOverrides::default(),
+   loader_overrides,
    normalized_remote,
    remote_auth_token,
)
```

#### 作用和细节

- 原始版本无论 CLI 传什么，TUI 都收到 `LoaderOverrides::default()`，所以无法生效外部配置文件。
- 改动覆盖了多个进入 TUI 的分支，避免普通启动、remote 子命令路径行为不一致。
- `loader_overrides_from_cli()` 只设置 `user_config_path`，其它 managed/system/test overrides 保持默认，避免意外改变企业 managed config 或测试 harness 行为。

### 3. `config/src/state.rs`

#### 改动点

`LoaderOverrides` 新增 `user_config_path`。

#### 代码 diff

```diff
pub struct LoaderOverrides {
    pub managed_config_path: Option<PathBuf>,
    pub system_config_path: Option<PathBuf>,
    pub system_requirements_path: Option<PathBuf>,
+   pub user_config_path: Option<PathBuf>,
    ...
}
```

```diff
Self {
    managed_config_path: Some(base.join("managed_config.toml")),
    system_config_path: Some(base.join("config.toml")),
    system_requirements_path: Some(base.join("requirements.toml")),
+   user_config_path: None,
    ...
}
```

#### 作用

这是 `--api_config` 能进入 config loader 的数据结构入口。默认值是 `None`，所以不传参数时完全保持原始行为。

### 4. `config/src/loader/mod.rs`

#### 改动点

读取用户 config 层时，如果存在 `overrides.user_config_path`，就用该绝对路径；否则仍读取 `$CODEX_HOME/config.toml`。

#### 代码 diff

```diff
-let user_file = AbsolutePathBuf::resolve_path_against_base(CONFIG_TOML_FILE, codex_home);
+let user_file = match overrides.user_config_path.as_ref() {
+    Some(path) => AbsolutePathBuf::from_absolute_path(path.clone())?,
+    None => AbsolutePathBuf::resolve_path_against_base(CONFIG_TOML_FILE, codex_home),
+};
```

#### 作用和细节

- `AbsolutePathBuf::from_absolute_path(path.clone())?` 要求传入的是绝对路径。如果传相对路径，配置加载会失败，而不是悄悄按当前目录解析。
- 只替换 user layer 的文件路径。`codex_home` 仍用于发现 auth、sessions，以及其它基于 `$CODEX_HOME` 的状态文件。
- `ignore_user_config` 分支仍然保留 user layer metadata；区别只是 metadata 指向指定的 `api_config` 文件。

### 5. `tui/src/lib.rs` 和 `/resume` 的关系

`--api_config` 如果只在启动时生效，但 `/resume` 时重建 config 丢掉它，就会复现 `Model provider 'uniapi' not found`。因此 `/resume` 修复里的 `loader_overrides` 传递同时也是 `--api_config` 的必要补丁。

关键链路是：

```text
cli --api_config=... 
  -> CliConfigOverrides.api_config
  -> LoaderOverrides.user_config_path
  -> codex_tui::run_main(..., loader_overrides, ...)
  -> load_config_or_exit(..., loader_overrides, ...)
  -> App { loader_overrides }
  -> rebuild_config_for_cwd().loader_overrides(self.loader_overrides.clone())
```

如果缺少最后两步，刚启动直接对话正常，但 `/resume` 或运行期 reload 后就找不到外部配置文件里的 provider。

另外，由于 `CliConfigOverrides` 新增了 `api_config` 字段，所有手动构造该结构的地方都必须显式填值。普通 TUI 的本地 model 准备路径和 app-server test client 使用 `api_config: None`，保持原来的测试/内部路径行为：

```diff
let overrides_cli = codex_utils_cli::CliConfigOverrides {
    raw_overrides,
+   api_config: None,
};
```

```diff
let cli_kv_overrides = CliConfigOverrides {
+   api_config: None,
    raw_overrides: config_overrides.to_vec(),
}
```

### 配置示例

```toml
model = "deepseek-v4-pro"
model_provider = "uniapi"

[model_providers.uniapi]
name = "UniAPI"
base_url = "https://example.com/v1"
env_key = "UNIAPI_API_KEY"
wire_api = "chat"
```

启动示例：

```bash
codex --api_config=/Users/a0000/path/to/api_config.toml
```

需要注意：`--api_config` 文件路径应使用绝对路径；API key 仍按 `env_key` 从环境变量读取，或者沿用项目已有 auth/provider 机制。

## 验证命令

已执行并通过：

```bash
just fmt
cargo test -p codex-tui app_server_session::tests::thread_resume_params_do_not_override_model_or_provider_for_embedded_sessions
cargo test -p codex-tui app::config_persistence::tests::rebuild_config_for_cwd_preserves_loader_overrides
```

也执行过：

```bash
cargo clippy --fix --tests --allow-dirty --allow-no-vcs -p codex-tui
```

该命令通过，但由于当前目录没有 VCS 元数据，`cargo clippy --fix` 会自动改一批无关样式点；这些无关改动已手动回退，只保留本文档列出的 `/resume` 修复相关改动。

## 补充说明（2026-05-17 排查实录）

### `tui/src/main.rs`（`codex-tui` 二进制）未处理 `--api_config`

`codex-tui` 是 tui crate 的独立二进制（`[[bin]] name = "codex-tui" path = "src/main.rs"`），其 `main.rs` 虽然通过 clap 接收了 `CliConfigOverrides`（含 `--api_config` 字段），但从未调用 `loader_overrides_from_cli()` 把 `api_config` 映射为 `LoaderOverrides.user_config_path`。

**后果**：直接运行 `codex-tui --api_config=...` 时，参数被静默忽略，仍使用 `$CODEX_HOME/config.toml`。

**修复方向**：参考 `cli/src/main.rs` 中的做法，在 `tui/src/main.rs` 调用 `run_main()` 前加上：

```rust
let loader_overrides = codex_config::LoaderOverrides {
    user_config_path: config_overrides.api_config.clone(),
    ..Default::default()
};
```

并将 `loader_overrides` 传入后续的 `run_main()` / 配置加载函数。

### 二进制选择：避免跑错版本

`--api_config` 的完整链路在 `cli/src/main.rs` 中实现。用户常犯的错误：

- 直接运行 `codex-tui`（绕过 CLI 层，`--api_config` 无效）
- 运行 Homebrew / 系统安装的旧版 `codex`（没有 `--api_config` 参数）

排查命令：

```bash
which codex       # 确保指向编译产出，非 /opt/homebrew/bin/codex
codex --help | grep api_config  # 确认参数存在
```

推荐统一使用 `codex`（cli crate 二进制）作为入口，它会内部调用 `codex_tui::run_main()`。


## D. `metadata = "local"` —— 本地模型元数据

2026-05-17 新增。第三方 provider 的 `/v1/models` 返回格式与 Codex `ModelsResponse` 不兼容，
导致反序列化失败 → 空列表 → fallback metadata → 警告。本改动允许用户在 provider 配置中用
`metadata = "local"` 直接在 TOML 里定义模型元数据，完全跳过远程 `/v1/models` 请求。

### 实施顺序

在 A–C 全部就位后做这批改动。不影响已完成的其它功能。

### 1. `model-provider-info/src/lib.rs`

#### 改动点

新增 `ModelMetadataSource` 枚举、`LocalModelInfo` 结构体及 `From<LocalModelInfo> for ModelInfo` 转换；
`ModelProviderInfo` 新增 `metadata` 和 `models` 字段；补齐 import 和 3 个构造点。

#### 代码 diff

```diff
--- a/model-provider-info/src/lib.rs
+++ b/model-provider-info/src/lib.rs
@@ -19,6 +19,15 @@
 use schemars::JsonSchema;
 use serde::Deserialize;
 use serde::Serialize;
+use codex_protocol::openai_models::ConfigShellToolType;
+use codex_protocol::openai_models::InputModality;
+use codex_protocol::openai_models::ModelInfo;
+use codex_protocol::openai_models::ModelVisibility;
+use codex_protocol::openai_models::ReasoningEffortPreset;
+use codex_protocol::openai_models::TruncationMode;
+use codex_protocol::openai_models::TruncationPolicyConfig;
+use codex_protocol::openai_models::WebSearchToolType;
+use codex_protocol::config_types::ReasoningSummary;
 use std::collections::HashMap;
 use std::fmt;
 use std::time::Duration;
```

在 `WireApi` 的 `Deserialize` impl 之后、`/// Serializable representation of a provider definition.` 之前插入：

```diff
+/// Where model metadata (context window, truncation policy, reasoning levels,
+/// etc.) comes from.
+#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, JsonSchema)]
+#[serde(rename_all = "lowercase")]
+pub enum ModelMetadataSource {
+    #[default]
+    Remote,
+    Local,
+}
+
+impl fmt::Display for ModelMetadataSource { /* "remote" / "local" */ }
+impl<'de> Deserialize<'de> for ModelMetadataSource { /* "remote" / "local" */ }
+
+pub struct LocalModelInfo {
+    pub slug: String,
+    #[serde(default)] pub display_name: Option<String>,
+    #[serde(default)] pub context_window: Option<i64>,
+    #[serde(default)] pub truncation_policy: Option<TruncationPolicyConfig>,
+    #[serde(default)] pub supported_reasoning_levels: Vec<ReasoningEffortPreset>,
+    #[serde(default = "default_true")] pub supports_parallel_tool_calls: bool,
+    #[serde(default)] pub supports_reasoning_summaries: bool,
+}
+
+fn default_true() -> bool { true }
+
+impl From<LocalModelInfo> for ModelInfo {
+    fn from(local: LocalModelInfo) -> Self {
+        // display_name 默认 = slug
+        // truncation_policy 默认 = TruncationMode::Bytes, limit = context_window.unwrap_or(128_000)
+        // supported_reasoning_levels 默认 = Low/Medium/High
+        // shell_type = ShellCommand, visibility = List, input_modalities = [Text]
+        // supports_search_tool = false, effective_context_window_percent = 90
+        // … 其余字段均为合理默认值
+    }
+}
```

`ModelProviderInfo` 末尾新增两个字段：

```diff
     pub supports_websockets: bool,
+    /// Where model metadata comes from. Defaults to `remote`.
+    #[serde(default)]
+    pub metadata: ModelMetadataSource,
+    /// User-supplied model catalog used when `metadata = "local"`.
+    #[serde(default)]
+    pub models: Vec<LocalModelInfo>,
 }
```

3 个构造点（`create_openai_provider`、`create_amazon_bedrock_provider`、`create_oss_provider_with_base_url`）各补：

```diff
+            metadata: ModelMetadataSource::Remote,
+            models: Vec::new(),
```

### 2. `model-provider/src/provider.rs`

#### 改动点

`models_manager()` 函数：当 `metadata == Local` 时从本地 models 构建 `StaticModelsManager`，跳过远程请求。

#### 代码 diff

```diff
--- a/model-provider/src/provider.rs
+++ b/model-provider/src/provider.rs
@@ -6,6 +6,8 @@
+use codex_model_provider_info::LocalModelInfo;
+use codex_model_provider_info::ModelMetadataSource;
 use codex_model_provider_info::ModelProviderInfo;
```

```diff
     fn models_manager(
         &self,
         codex_home: PathBuf,
         config_model_catalog: Option<ModelsResponse>,
     ) -> SharedModelsManager {
+        if self.info.metadata == ModelMetadataSource::Local {
+            let local_models: Vec<codex_protocol::openai_models::ModelInfo> = self
+                .info.models.iter().cloned().map(LocalModelInfo::into).collect();
+            let catalog = ModelsResponse { models: local_models };
+            return Arc::new(StaticModelsManager::new(
+                self.auth_manager.clone(), catalog,
+            ));
+        }
+
         match config_model_catalog {
```

### 3. `config/src/thread_config/remote.rs`

#### 代码 diff

```diff
--- a/config/src/thread_config/remote.rs
+++ b/config/src/thread_config/remote.rs
@@ -4,6 +4,7 @@
+use codex_model_provider_info::ModelMetadataSource;
```

```diff
         requires_openai_auth: provider.requires_openai_auth,
         supports_websockets: provider.supports_websockets,
+        metadata: ModelMetadataSource::Remote,
+        models: Vec::new(),
     };
```

### 4. `model-provider-info/src/model_provider_info_tests.rs`

新增 3 个测试（略，详见源文件末尾 `test_metadata_defaults_to_remote`、`test_metadata_local_with_models`、`test_metadata_remote_is_default_for_existing_configs`）。

### 配置示例

```toml
[model_providers.dsapi]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
env_key = "dsoff_KEY"
wire_api = "chat"
metadata = "local"

[[model_providers.dsapi.models]]
slug = "deepseek-v4-flash"
display_name = "DeepSeek V4 Flash"
context_window = 1000000
truncation_policy = { mode = "bytes", limit = 900000 }

[[model_providers.dsapi.models]]
slug = "deepseek-v4-pro"
display_name = "DeepSeek V4 Pro"
context_window = 1000000
```

### 验证命令

```bash
cargo check -p codex-model-provider-info -p codex-model-provider -p codex-config
cargo check -p codex-cli
```
