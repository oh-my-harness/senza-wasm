# senza-wasm M2+M3 设计：config 透传 + 成本快照 + 工具审批门

日期：2026-09-05
状态：已实施（commit 39240d9 / 45ef51e / 193d460；npm 包项留独立 spec）
前置：M1 已完成（`docs/superpowers/specs/2026-09-05-senza-wasm-session-core-design.md`）

## 1. 背景

M1 落地了会话核心（历史续接、session 导出导入、多轮工具、maxTurns
守卫）。M2 补齐宿主可控的运行配置与成本可见性；M3 在工具执行前加
宿主审批门。两者都不动内核——所需机制内核全部已有：

- `LoopConfig.response_format: Option<ResponseFormat>`（loop/src/config.rs:150，re-export 自 llm_adapter）
- `LoopConfig.final_answer_mode: FinalAnswerMode`（loop/src/final_answer.rs:12：`Heuristic` / `Tool(config)`）
- `AssistantMessage.usage: Option<TokenUsage>`（types/src/messages.rs:76，每个 assistant 消息带 provider 上报的用量）
- `HookedTool` + `BeforeToolCallHook`（`BeforeToolCallDecision::Allow/Modify/Deny`，types/src/hooks.rs:144）

npm 包发布（M2 原第 6-9 项）**单独延期**：wrapper/.d.ts 值得独立
spec（类型层设计、双产物、publish 流水线），且不阻塞 M3。本 spec
覆盖 M2 的 1-5 项 + M3 全部。

## 2. 需求

### 2.1 M2：config 透传

1. `AgentOpts` 增加（constructor JSON，camelCase）：
   - `responseFormat`: `"json_object"` | `{ "json_schema": { "name": string, "schema": object, "strict"?: boolean } }`
   - `finalAnswerMode`: `"heuristic"`（默认）| `"required_tool"` | `"tool_with_text_fallback"`
   - `streamIdleTimeoutMs`: number（现固定 5000）
   - `thinkingLevel`: `"off"`（默认）| `"low"` | `"medium"` | `"high"`（透传 `ThinkingLevel`）
2. `build_config` 接收 `AgentOpts` 的这些字段而非硬编码。
3. costSnapshot()：`EmbeddedAgent::cost_snapshot() -> String`，返回
   累计 JSON（见 §3.2）。
4. `stream_timeout_ms` 真 key 实测：**不在本 spec**——需要真实
   provider key，属手动验收，另行走查。

### 2.2 M3：工具审批门

1. `registerTool` 增加可选第 5 参 `options_json`：
   `{ "approval": "auto" | "manual" }`（默认 auto）。**不改现有
   签名顺序**——callback 之后追加。
2. manual 工具：执行暂停在审批门，宿主调
   `approveToolCall(tool_use_id, allow: boolean)` 放行或拒绝。
3. 拒绝语义：工具结果为 `ToolFailure`（`"denied_by_host"`），走
   内核既有 Deny 路径——失败对 LLM 可见（模型可据此调整），事件流
   出 `tool_execution_end` 带 error。
4. 超时：审批门不设内核超时（内核无此概念）；宿主侧
   `streamIdleTimeoutMs` 与 maxTurns 守卫已是兜底。悬而未决的
   审批让 run 挂起直到宿主决定或 cancel——与 pump 模式语义一致。
5. 与 pump 模式正交：manual + callback=null 组合有效（先审批，
   放行后进 pump 等待 resolveTool）。

## 3. 设计

### 3.1 AgentOpts 扩展（serde 形状）

```rust
#[serde(default, rename = "responseFormat")]
response_format: Option<FacadeResponseFormat>,   // 枚举，见下
#[serde(default, rename = "finalAnswerMode")]
final_answer_mode: FacadeFinalAnswerMode,        // 默认 heuristic
#[serde(default, rename = "streamIdleTimeoutMs")]
stream_idle_timeout_ms: u64,                     // 默认 5000
#[serde(default, rename = "thinkingLevel")]
thinking_level: FacadeThinkingLevel,             // 默认 off
```

`FacadeResponseFormat`（serde tag，camelCase 字段）：

```rust
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
enum FacadeResponseFormat {
    JsonObject,
    JsonSchema { name: String, schema: serde_json::Value, strict: Option<bool> },
}
```

映射到内核 `ResponseFormat::JsonObject / JsonSchema { name, schema, strict }`。

`FacadeFinalAnswerMode`：字符串枚举
`heuristic | required_tool | tool_with_text_fallback`，映射到
`FinalAnswerMode::Heuristic / required_tool() / tool_with_text_fallback()`。

`FacadeThinkingLevel`：字符串枚举
`off | minimal | low | medium | high | xhigh | budget:N`（内核变体
`Off/Minimal/Low/Medium/High/XHigh/Budget(u32)`，llm-api-adapter
types/thinking.rs:18——已核实）。serde 自定义反序列化：前 6 个
字符串直接对应，`"budget:1024"` 解析为 `Budget(1024)`。

`AgentDeps` 增加对应字段；`build_config` 改为吃这些值。mock provider
忽略 responseFormat/thinkingLevel（MockLlmClient 不消费），但
finalAnswerMode 影响 loop 行为（required_tool 时未调工具的终轮会
Error）——mockScript 可测。

### 3.2 costSnapshot()

不引入内核 `CostAggregate`（其 by_model/计费画像/价格表对门面过重，
且门面无价格数据）。**门面自累计**：事件循环里从
`AgentEvent::MessageEnd { message }`（以及 TurnEnd 的 message）提取
`message.usage`，按字段累加到 `AgentState.cost`：

```rust
struct CostState {
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,     // TokenUsage.cache_creation_tokens
    reasoning_tokens: u64,
    provider_calls: u64,         // 有 usage 的 message 数
}
```

`cost_snapshot() -> String` 返回：

```json
{"totalInputTokens":10,"totalOutputTokens":5,"totalCacheReadTokens":0,
 "totalCacheWriteTokens":0,"totalReasoningTokens":0,"providerCalls":1}
```

字段 camelCase，与事件风格一致。**不乘价格**——没有单价表的假
成本比没有成本更糟；宿主拿 token 数自己算钱。clear/import 不重置
cost（成本是进程生命周期累计）；`clearSession` 语义只清对话。若
需要重置，宿主重建 agent（成本快照本来就是诊断用途）。

usage 提取点：`MessageEnd`（assistant message 完整时）最可靠——
TurnEnd 也带 message，但 MessageEnd 每轮必有且去重简单（按
message_id 防重）。实现：事件循环里 match MessageEnd，按
message_id 集合去重后累加。

### 3.3 审批门（M3）

复用 M1 的 PendingTool 停泊模式，组合而非改造：

```rust
// tools.rs 新增
pub struct ApprovalGate {
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,  // tool_use_id -> allow
}
```

manual 工具的执行链：`HookedTool { inner: 原工具, before: gate_hook }`。
gate_hook 实现 `BeforeToolCallHook`：在 `ctx.tool_use_id` 下停泊
oneshot receiver，等 `approveToolCall(id, allow)`：
- allow → `BeforeToolCallDecision::Allow`
- deny → `BeforeToolCallDecision::Deny(ToolFailure::new("denied_by_host", ...))`

`approveToolCall` 遍历 `pump_by_name` 找到持有该 id 的 gate（与
resolveTool 同模式），触发 sender。未知 id 报 JsValue 错（与
resolveTool 文案一致模式）。

**并发审批**：一个 run 里多个 manual 工具并行（Parallel 模式）时
各自停泊自己的 oneshot，互不阻塞。

**注册形状**：`register_tool(name, desc, schema, callback, options_json)`——
wasm_bindgen 不支持可选尾参的重载，加一个带 options 的入口：

```rust
#[wasm_bindgen(js_name = registerTool)]
pub fn register_tool(&self, name, description, schema_json, callback) -> ...  // 既有，auto

#[wasm_bindgen(js_name = registerToolWithOptions)]
pub fn register_tool_with_options(&self, name, description, schema_json,
    callback, options_json: String) -> ...   // { "approval": "manual" }
```

（wasm-bindgen 对同名导出多签名会生成重载，实测可行；若工具链
不支持则退化为 registerToolWithOptions 单独入口。）

### 3.4 事件流新增

审批门不新增事件类型——`tool_execution_start` 已在执行前发出，
Deny 后 `tool_execution_end` 带 failure。宿主 UI 靠"start 之后
既无 update 也无 end"识别挂起中的审批（与 pump 相同的判据）。
**不与 Python SDK 对齐问题**：Senza Python SDK 无审批门，本仓库
不发明新事件——保持事件面不动，用既有事件组合表达。

## 4. 测试计划

| 层 | 内容 | 断言 |
|---|---|---|
| wasm 单测 | responseFormat/finalAnswerMode/thinkingLevel 解析 | 反序列化形状 + build_config 透传 |
| wasm 单测 | cost 累计：两轮 mock 文本对话 | provider_calls=2、token 数=两条 usage 之和；message_id 去重 |
| wasm 单测 | costSnapshot JSON 字段 | camelCase 键名 |
| Node 冒烟 | finalAnswerMode=required_tool + 纯文本 script | error 事件（未调 final_answer 工具）|
| Node 冒烟 | manual 工具 approve 路径 | start → approveToolCall(id,true) → end → agent_end |
| Node 冒烟 | manual 工具 deny 路径 | start → approveToolCall(id,false) → end 带 denied_by_host → loop 继续（下一轮文本）→ agent_end |
| Node 冒烟 | 未知 id approveToolCall | JsValue 错误 |
| mock 增强 | mockScript text 项支持 usage 注入 | `{kind:"text",text:"…",usage:{inputTokens:10,outputTokens:5}}` |

mockScript usage 注入（已核实）：内核有
`MockResponse::with_reported_usage(Usage)` builder（test_utils.rs:247，
替换 terminal MessageStop 的 usage）。MockSpec 的 text 变体加可选
`usage: {inputTokens, outputTokens, cachedInputTokens?,
cacheCreationInputTokens?, reasoningTokens?}` 字段，映射到
`Usage` 后 `.with_reported_usage(...)`。

## 5. 风险

| 风险 | 缓解 |
|---|---|
| ThinkingLevel 变体名与假设不符 | **已核实**（types/thinking.rs:18，spec §3.1 已列全）；映射表一处集中 |
| cost 从 MessageEnd 提取漏 TurnEnd-only 路径 | MessageEnd 每轮必发（内核契约）；wasm 测试覆盖两轮 |
| approval 与 pump 组合的状态机复杂度 | gate 与 pump 是两个独立停泊点，顺序执行（gate→pump），无交叉状态 |
| mockScript usage 注入依赖内核 test_utils 细节 | **已核实**：`with_reported_usage` builder 存在（test_utils.rs:247） |
