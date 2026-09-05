# senza-wasm 会话核心设计（M1）

日期：2026-09-05
状态：待评审
前置：v0.1 已完成（`2026-09-03-senza-wasm-design.md`，Senza 仓库）；
本 spec 覆盖重新规划后的 M1，后续 M2–M4 及 roadmap 重排见
`ROADMAP.md`（与本 spec 一同更新）。

## 1. 背景与问题

v0.1 门面是**单轮失忆**的：`src/agent.rs` 中 `prompt()` 每次以
`messages: vec![]` 构造 `AgentContext`，同一 agent 实例的第二次提问
不携带任何历史。这不是"未暴露的内核能力"，而是功能空洞——npm
发布后用户第一个问题就会是"怎么连续对话"。

**2026-09-05 内核实证追加：v0.1 还有一个更深的 bug。**
`agent_loop` 从不读 `LoopConfig.run.initial_messages`——初始消息
由 harness 层（`loop_driver.rs:373-374` 的
`session.build_context() + initial` 链）合并进 `ctx.messages`，
裸 `agent_loop` 调用方必须自带。门面 `prompt()` 传的却是
`messages: vec![]`，用户文本**从未进入对话**。mock 冒烟能过
纯粹因为 `MockLlmClient` 不检查请求内容；真 key 路径下模型会
收到空消息列表（provider 400 或空响应）。v0.1 遗留的"真 key
路径未实测"一项，其严重度由"未测"升级为"必坏"。

同时，测试基建有一个堵点：mock 分支硬编码单条 text 响应，JS 侧
无法让 mock 吐 tool_call，导致 spec §4.4 的"多轮工具对话冒烟"
（v0.1 验收项，至今未做）在当前门面上**不可能编写**。

三件事互为依赖：修用户消息注入需要历史续接才完整；没有 mock
script 测不了续接；没有续接 mock script 只能测失忆 agent。
故合并为一个 milestone。

## 2. 目标与非目标

**目标**

1. `EmbeddedAgent` 具备对话连续性（历史续接、重置、导出/导入）
2. mock provider 可从 JS 配置多轮剧本（text / tool_use / 错误）
3. 多轮工具对话冒烟（pump + JsTool 两条路径）+ 浏览器真机冒烟
4. pump 路径 call_id 语义与 Python SDK 对齐复查（原 v0.3 未定项）

**非目标**

- Session 分支 / tree_ops / JSONL 持久化——宿主职责，用
  exportSession 的 JSON 承载
- structured output / finalAnswerMode / cost snapshot（M2）
- 工具审批门（M3）、loop_safety（M4，上游拆分后消费）
- npm 包裹层与发布（M2 之后）

## 3. 设计

### 3.1 门面状态变更

`AgentDeps`（不可变 wiring）与 `AgentState`（可变运行态）之外，
新增 `AgentSession`——**对话历史容器**，锁内可变：

```rust
pub(crate) struct AgentSession {
    /// 上一次 run 结束后的完整消息列表（AgentContext.messages）。
    /// prompt() 从这里续接。
    history: Vec<AgentMessage>,
}
```

- `prompt(text)`：构造 user `AgentMessage`，
  `AgentContext { system_prompt, messages: history + [user_msg] }`
  起跑（初始消息必须由调用方注入 ctx——见 §1 实证，
  `agent_loop` 不读 `config.run.initial_messages`，该字段仅作
  run 元数据）。
- **合并语义（已实证为增量）**：内核 `loop_fn.rs:264` 的
  `new_messages` 只收 run 期间生成的 assistant / tool_result /
  steer 消息，不含初始用户消息（harness 层对 initial 是单独
  persist 的）。故 run 正常结束（AgentEnd）时：
  `history = history + [user_msg] + new_messages`。
- **事件循环读完 Error 后的 AgentEnd**：内核契约是 Error 后
  AgentEnd 立即到达（types/events.rs:96）。门面当前在 Error 就
  break，收不到收尾事件。M1 改为 break 条件只认 AgentEnd，
  Error 仅入队不终止（AgentEnd 兜底终止，防内核契约变化再加
  `!running` 守护）。
- **Error 不入史**：Error 时 run 内已生成的半成品 assistant
  消息在内核侧只存在于局部 `new_messages`（未发出即丢弃），
  门面合并不到，天然满足"宁可丢半轮，不可脏历史"。用户消息
  同样不保留——失败的提问不占历史，宿主重试即可。
- **并发守护**：`prompt()` 在 `running == true` 时直接入队一条
  `error` 事件（`error_type: "busy"`），不启动新 run。当前门面
  对重复 prompt 的行为是未定义的（两个 spawn_local 竞争同一队列），
  M1 一起修掉。

### 3.2 会话 API（新增三个导出）

```webidl
agent.exportSession(): string       // JSON: {history: [...]}，AgentMessage 序列化
agent.importSession(json: string): void   // 解析失败 → JsValue error
agent.clearSession(): void          // history 清空；不中断进行中的 run
```

- 格式是**门面自有契约**（版本字段 `v: 1`），不直接吐内核
  `AgentMessage` 的 serde 格式——内核 rev 升级时字段变了，门面做
  迁移而不是让宿主 JSON 坏掉。
- 序列化用 `serde_json` 手工映射到稳定形状（role + content blocks），
  未知块类型打平为 text 兜底。这是本 spec 最大的新增代码面，
  预估 100–150 行（含往返测试）。
- `clearSession` 不动 running run。为防"clear 后 run 结束又把
  旧消息写回 history"，spawn 闭包捕获 run 开始时的 history
  快照 + 代次号（generation counter）：合并回写仅当代次一致，
  否则丢弃。M1 直接实现（不留给实测），代价一行。

### 3.3 mock script（测试基建，非公开 API 面）

`AgentOpts` 新增可选字段，仅 `provider: "mock"` 时生效：

```jsonc
{
  "provider": "mock",
  "model": "mock-model",
  "mockScript": [
    {"kind": "text", "text": "hello"},
    {"kind": "tool_use", "toolUseId": "t1", "name": "echo", "args": "{\"x\":1}"},
    {"kind": "rate_limit_error"}
  ]
}
```

- serde tag 枚举映射到内核 `MockResponse::text / tool_use /
  rate_limit_error`（`test_utils`，rev `358af9f` 已有，无需上游改动）
- `stall_after / slow_stream / stream_dropped_mid` **不进**——
  watchdog 场景已有 wasm-smoke 运行门覆盖，script 保持最小
- mock 分支保留无 script 的默认行为（单条 "mock response" text），
  既有 node_smoke 不动

### 3.4 call_id 语义复查（原 v0.3 未定项）——已结案

**复查结论：保持 `tool_use_id`，无需对齐。** 实证：Senza Python SDK
（`Senza/src/shared/event_stream.rs:199`）事件寻址字段就是
`tool_use_id`，两侧一字不差；Python SDK 没有 pump 模式（只有
callback，`pytool.rs`），不存在独立 call_id 概念。

附带修正（实施时发现）：v0.1 的 pump 通道 key 是门面生成的随机
uuid，而 `resolveTool` 按 LLM 分配的 `tool_use_id` 寻址——两者
永不相等，pump 模式在 wasm 上从未真正工作过。M1 重设计为
`PendingTool` 内部 `HashMap<tool_use_id, oneshot sender>`：
`execute()` 在 `ctx.tool_use_id` 下停泊一次性通道，`resolve()`
按该 id 触发（src/tools.rs，测试
`pending_tool_resolves_after_host_reply`）。

## 4. 测试计划

| 层 | 内容 | 断言 |
|---|---|---|
| wasm-bindgen-test | export/import 往返、并发 prompt 守护、session 格式版本、Error→AgentEnd 双事件序列 | 7 个既有 + 新增（以实测计数为准） |
| Node 冒烟 | mock script：text→text 两轮对话，断言第二次 `chat_stream` 请求含第一轮 user+assistant（mock `captured_requests` 或事件序列佐证） | 历史续接 + 用户消息注入修复 |
| Node 冒烟 | script=[tool_use, text]，callback=null：tool_execution_start → resolveTool → 下一 turn_start → text_delta → agent_end | 完整多轮 pump 序列 |
| Node 冒烟 | 同 script + JS 闭包工具 | 单 prompt 两 turn 自动完成 |
| Node 冒烟 | 反例：resolveTool 未知 id 报错；全 tool_use script + maxTurns=2 → resource_limit | 错误路径 |
| 浏览器 | `tests/browser_smoke.html` 跑同款多轮断言，headless Chromium 实测 | bundler 产物真机 |

- 浏览器冒烟**暂不进 CI**（playwright/chromedriver 依赖不值当），
  本地实测通过即入库为手动门
- Node 冒烟扩展为独立文件 `tests/node_multi_turn.mjs`，与既有
  `node_smoke.mjs` 并列，CI 两个都跑

## 5. 风险

| 风险 | 缓解 |
|---|---|
| ~~AgentEnd 的 `new_messages` 只含增量而非全量~~ | **已实证为增量**（loop_fn.rs:264，见 §3.1），合并语义按增量设计 |
| 半轮脏历史（Error 前的 partial assistant 消息） | **已实证**：内核 Error 路径不发出未完成的 new_messages，门面天然丢弃；测试覆盖 Error→AgentEnd 序列 |
| session JSON 手工映射漏 content block 类型 | 往返测试覆盖全部 17 事件变体涉及的块类型；未知类型 text 兜底 |
| mock script 泄漏进公开 API 文档 | `#[wasm_bindgen]` 导出面不变，仅 constructor JSON 多一个字段；文档标注测试用途 |

## 6. 验收标准

1. `cargo clippy --target wasm32-unknown-unknown -- -D warnings` +
   `cargo fmt --check` 通过
2. wasm-bindgen-test 全绿（≥11 个）
3. `node tests/node_smoke.mjs` + `node tests/node_multi_turn.mjs`
   全绿，后者含两轮历史续接断言
4. headless Chromium 跑 `browser_smoke.html` 输出 PASS
5. call_id 复查结论落档（本 spec §3.4 或 ROADMAP）
