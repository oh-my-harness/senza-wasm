# ROADMAP

senza-wasm：把 llm-harness 内核（loop / 流式 / 工具 / 断路器）以 WebAssembly
交付给 TypeScript 生态。设计 spec 与实施计划在 Senza 仓库
（`docs/superpowers/specs/2026-09-03-senza-wasm-design.md`、
`docs/superpowers/plans/2026-09-04-senza-wasm-implementation.md`），
本文件只追踪这个仓库自己的进度与下一步。

当前状态标注（2026-09-04）：

- ✅ 已完成并实测验证
- 🚧 进行中
- ⬜ 未开始

## v0.1 — 内核通行证 + 门面（当前）

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
| **GitHub 远端仓库 + CI 跑起来** | ⬜ | 本地提交 `2d5e20b` 待推送 |
| **`stream_timeout_ms`（连接/stream 超时）真 key 路径实测** | ⬜ | mock 路径已覆盖 watchdog；真 reqwest-wasm fetch 路径未测 |

## v0.2 — 可发布（npm 包）

| 项 | 状态 | 说明 |
|---|---|---|
| `npm/` 包裹层：判别联合类型 + `AgentEvent` TS 类型 | ⬜ | spec §4.2：`.wasm` 导出面保持原始 JSON 行，类型在包裹层 |
| `.d.ts` 完整 + `tsc --strict` 通过 | ⬜ | spec §4.4 验收之一 |
| wasm-pack 双产物 CI（bundler + nodejs） | ⬜ | spec §4.3；当前 ci.yml 只构建 nodejs |
| npm 发布流水线（包名 `senza-wasm`，scope 待定 — spec 未定项 1） | ⬜ | 版本跟随内核 rev，`0.x` 起步 |
| 浏览器 demo（README 级：单页 chat + 一个工具） | ⬜ | spec §4.4；key 暴露约束见 §5 风险表 |
| 多轮工具对话冒烟（tool_call → resolveTool → 下一轮） | ⬜ | spec §4.4 事件序列断言的完整版 |

## v0.3 — 内核子集扩展

| 项 | 状态 | 说明 |
|---|---|---|
| `poll()` 返回 `string[]` vs `ArrayBuffer` 定夺 | ⬜ | spec 未定项 2，等实测事件吞吐 |
| loop_safety 策略族进 wasm（不依赖 sandbox/文件系统的部分） | ⬜ | spec 未定项 3，倾向首版不进 |
| `registerTool` pump 路径的 call_id 语义与 Python SDK 对齐复查 | ⬜ | 当前用 `tool_use_id`（LLM 分配 id）寻址 |
| Anthropic provider 进门面 | ⬜ | spec §4.2 webidl 已列，门面当前只有 openai + mock |

## v1.0 — 第二宿主

| 项 | 状态 | 说明 |
|---|---|---|
| Unity / Puerts 插件（fetch shim、C# bridge） | ⬜ | 独立 spec，不在此仓库 |
| wasm 二进制体积优化（gzip + 按需 feature 裁剪） | ⬜ | spec §5：首版接受 reqwest+rustls 全家桶 |

## 约束（每个 milestone 都继承）

- 内核 crates 零 wasm-bindgen 痕迹——绑定全部在本仓库（spec §2 决策 4）
- 内核以 git rev 锁定；**不得用 path 依赖开发**（path 依赖会把 runtime
  workspace 的 feature unification 拖进来，mio/rusqlite 等 native 依赖
  污染 wasm 编译——2026-09-04 实测）
- 事件 `type` 字符串与 Senza Python SDK 一字不差；新增事件先改
  Python 侧映射，再同步这里
- 浏览器仅开发/演示；生产 key 走代理网关（spec §5）
