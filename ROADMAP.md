# ROADMAP

senza-wasm：把 llm-harness 内核（loop / 流式 / 工具 / 断路器）以 WebAssembly
交付给 TypeScript 生态。设计 spec 与实施计划在 Senza 仓库
（`docs/superpowers/specs/2026-09-03-senza-wasm-design.md`、
`docs/superpowers/plans/2026-09-04-senza-wasm-implementation.md`），
本仓库自有 spec 在 `docs/superpowers/specs/`（M1：
`2026-09-05-senza-wasm-session-core-design.md`）。

当前状态标注（2026-09-05）：

- ✅ 已完成并实测验证
- 🚧 进行中
- ⬜ 未开始

## v0.1 — 内核通行证 + 门面（已完成，遗留 2 项归入 M1/M2）

| 项 | 状态 | 说明 |
|---|---|---|
| 上游通行证：adapter `Provider` wasm 分叉 | ✅ | llm-api-adapter rev `33d3403`（PR oh-my-harness/llm-api-adapter#16） |
| 上游通行证：loop `agent_loop` Send 签名 cfg 分叉 | ✅ | runtime PR #189（合并为 `da5636f`） |
| 上游通行证：tokio time 桥（`time.rs` + gloo-timers） | ✅ | 同上；运行门实测 PASS，issue #180 关闭 |
| 上游通行证：`Tool::execute` wasm 分叉（`ToolFuture` 别名） | ✅ | runtime PR #190/#191（合并为 `358af9f`） |
| `EmbeddedAgent` 门面（createAgent / prompt / poll / cancel / registerTool / resolveTool） | ✅ | `src/agent.rs`，`spawn_local` 驱动，max_turns 门面级守护 |
| 事件 JSON 行序列化（17 变体，与 Python SDK 对齐） | ✅ | `src/events.rs`，`#[non_exhaustive]` catch-all → `unknown` |
| 工具双路径（JsTool 闭包 + PendingTool pump） | ✅ | `src/tools.rs`，mpsc take-once 语义 |
| wasm32 测试（events 序列化 + 工具桥） | ✅ | wasm-bindgen-test-runner 真机 7/7 |
| Node 冒烟（完整 mock turn + 事件序列断言） | ✅ | `tests/node_smoke.mjs`，release 构建验证 |
| clippy `-D warnings`（wasm32） | ✅ | CI 配置就位（`.github/workflows/ci.yml`） |

**v0.1 实测后追加（2026-09-05 内核实证，详见 M1 spec §1）**：

- **门面 prompt 文本从未进入对话**（`agent_loop` 不读
  `config.run.initial_messages`，门面传 `messages: vec![]`）——
  mock 冒烟因 MockLlmClient 不查请求内容而假绿。M1 修复。
- **真 key 路径必坏**（同上）——`stream_timeout_ms` 真 key 实测
  顺延到 M2，M1 落地后真 key 才有测试意义。

## M1 — 会话核心 + 测试基建（已完成，待推送 + 上游 wasm 修复合入）

spec：`docs/superpowers/specs/2026-09-05-senza-wasm-session-core-design.md`

| 项 | 状态 | 说明 |
|---|---|---|
| mock script（text / tool_use / rate_limit_error 序列） | ✅ | spec §3.3；复用内核 `MockResponse`，无上游改动 |
| 对话连续性（history + [user_msg] 续接，增量合并，busy 守护） | ✅ | spec §3.1；修复 prompt 文本注入 bug（commit b947cdc） |
| exportSession / importSession / clearSession + 代次计数 | ✅ | spec §3.2；宿主持久化，门面自有 JSON 契约（commit 1946260） |
| Error→AgentEnd 事件循环修正 | ✅ | spec §3.1；break 条件只认 AgentEnd |
| call_id 语义与 Python SDK 复查 | ✅ | spec §3.4 已结案：保持 `tool_use_id`；附带修复 v0.1 pump 通道 key 错位 bug |
| 多轮工具冒烟（pump + JsTool 路径 + 反例） | ✅ | spec §4；`tests/node_multi_turn.mjs` 进 CI（commit 8a6c3b3） |
| 浏览器真机冒烟（headless Chromium） | ✅ | spec §4；手动门，不进 CI；Edge 152 实测 PASS，`--target web`（commit b6e6955） |
| GitHub 远端仓库 + CI 首跑 | ⬜ | 前提项；本地提交待推送（含 `fix/wasm-tool-progress-spawn` 上游修复合入后重 pin） |

## M2 — config 透传 + 成本（已完成，spec 见 2026-09-05-senza-wasm-m2-m3）

| 项 | 状态 | 说明 |
|---|---|---|
| `responseFormat`（structured output）透传 | ✅ | json_object / json_schema{name,schema,strict} |
| `finalAnswerMode` 透传 | ✅ | heuristic / required_tool / tool_with_text_fallback |
| `thinkingLevel` / `streamIdleTimeoutMs` 透传 | ✅ | off…xhigh / budget:N；非法 thinkingLevel 报错 |
| `costSnapshot()` | ✅ | 门面自累计 token（不乘价格），message_id 去重；clear 不重置 |
| mockScript usage 注入 | ✅ | `with_reported_usage` 喂测试 |
| `stream_timeout_ms` 真 key 实测 | ⬜ | 需真实 provider key，手动验收另行走查 |
| npm/ 包裹层：判别联合类型 + `AgentEvent` TS 类型 | ⬜ | 独立 spec（类型层设计 + 双产物 + publish 流水线一并） |
| `.d.ts` 完整 + `tsc --strict` | ⬜ | 同上，随 npm 包 |
| wasm-pack 双产物 CI（bundler + nodejs） | ⬜ | 当前 ci.yml 只构建 nodejs |
| npm 发布流水线（包名/scope 待定） | ⬜ | 版本跟随内核 rev，`0.x` 起步 |
| 浏览器 demo（单页 chat + 一个工具） | ⬜ | M1 冒烟 html 可作底子 |
| `poll()` 返回 `string[]` vs `ArrayBuffer` 定夺 | ⬜ | 等 npm wrapper 类型层实测吞吐再议 |

## M3 — 工具审批门（已完成，spec 同 M2 文档）

| 项 | 状态 | 说明 |
|---|---|---|
| `registerToolWithOptions` + `approveToolCall(id, allow)` | ✅ | `{approval:"manual"}`；与 pump 正交组合 |
| 基于 HookedTool `BeforeToolCallHook`（Allow/Deny） | ✅ | ApprovalGate 停泊 oneshot；deny→`denied_by_host` failure（LLM 可见） |
| 审批流冒烟（approve / deny / 未知 id） | ✅ | node 3 段 + 浏览器 deny 段全过；超时=不设内核超时，idle/maxTurns/cancel 兜底（设计决策） |

## M4 — loop_safety 策略族（节奏取决于上游）

| 项 | 状态 | 说明 |
|---|---|---|
| 上游：loop_safety 从 strategy 拆为只依赖 types+loop 的 crate | ⬜ | strategy 依赖 llm-harness-agent（拖 sandbox/tempfile），需上游拆分；拆分方案届时与上游维护者讨论 |
| 消费：repetition / failure-breaker / death-spiral / truncation 进 wasm | ⬜ | 上游拆分后直接消费，本仓库不重写安全语义 |
| Anthropic provider 进门面 | ⬜ | 与 M4 无依赖，可提前 |

## v1.0 — 第二宿主

| 项 | 状态 | 说明 |
|---|---|---|
| Unity / Puerts 插件（fetch shim、C# bridge） | ⬜ | 独立 spec，不在此仓库 |
| wasm 二进制体积优化（gzip + 按需 feature 裁剪） | ⬜ | 首版接受 reqwest+rustls 全家桶 |

## 约束（每个 milestone 都继承）

- 内核 crates 零 wasm-bindgen 痕迹——绑定全部在本仓库（原 spec §2 决策 4）
- 内核以 git rev 锁定；**不得用 path 依赖开发**（path 依赖会把 runtime
  workspace 的 feature unification 拖进来，mio/rusqlite 等 native 依赖
  污染 wasm 编译——2026-09-04 实测）
- 事件 `type` 字符串与 Senza Python SDK 一字不差；新增事件先改
  Python 侧映射，再同步这里
- 浏览器仅开发/演示；生产 key 走代理网关（原 spec §5）
