# senza-wasm

[llm-harness](https://github.com/oh-my-harness/llm-harness-runtime) agent loop 的 TypeScript 绑定：把进程内引擎（agent loop、流式输出、工具调用、重试/熔断）编译成 WebAssembly，供浏览器与 Node 使用。

```
┌─────────────────────────────┐
│  你的应用 (TS / 浏览器 /     │
│  Node / Puerts)             │
│   new EmbeddedAgent(...)    │
│   .prompt() → poll() 事件流 │
└──────────────┬──────────────┘
               │ wasm-bindgen
┌──────────────▼──────────────┐
│  senza-wasm（本仓库）       │  ← 所有 #[wasm_bindgen] 都在这里
├─────────────────────────────┤
│  llm-harness-loop (Rust)    │  ← git rev 锁定，零绑定依赖
│  网络留在 Rust 侧           │    (reqwest wasm → fetch)
└─────────────────────────────┘
```

## 状态

M1（会话核心）已实现——设计文档见 `docs/superpowers/specs/2026-09-05-senza-wasm-session-core-design.md`，进度见 ROADMAP。历史续接、会话导出/导入、多轮工具调用（闭包 + pump 两种模式）、maxTurns 守卫均已落地并有测试覆盖。

## 本地构建

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack --locked

# Node 目标
wasm-pack build --target nodejs --out-dir pkg-node
node tests/node_smoke.mjs
node tests/node_multi_turn.mjs

# 浏览器目标（无需打包器 —— init + fetch）
wasm-pack build --target web --out-dir pkg-web
python3 -m http.server 8931   # 在仓库根目录
open http://localhost:8931/tests/browser_smoke.html

# wasm 单测（wasm-bindgen-test-runner；.cargo/config.toml 已配好 runner）
cargo test --target wasm32-unknown-unknown
```

> 注意：`bundler` 目标的胶水代码会 `import * as wasm from "./x.wasm"`，
> 依赖打包器——浏览器原生并未实现 wasm ESM integration。浏览器直连
> （不经打包器）场景请用 `--target web`。

## 用法

```ts
import init, { EmbeddedAgent } from "senza-wasm";

await init(); // web 目标需要；bundler 目标是静态导入

const agent = new EmbeddedAgent(JSON.stringify({
  provider: "openai",   // 或 "mock"（测试门面）
  apiKey: "...",        // 浏览器端仅限开发/演示——生产必须走网关代理
  model: "gpt-4o-mini",
  systemPrompt: "You are helpful.",
  maxTurns: 16,          // 门面级守卫；超出即中止，
                         // 发出 error 事件，error_type 为 "resource_limit"
  // —— M2 透传项 ——
  responseFormat: { kind: "json_object" },          // 或
  // responseFormat: { kind: "json_schema", name: "out",
  //   schema: {…}, strict: true },
  finalAnswerMode: "required_tool",  // heuristic（默认）| required_tool
                                     // | tool_with_text_fallback
  thinkingLevel: "high",             // off|minimal|low|medium|high|xhigh|budget:N
  streamIdleTimeoutMs: 5000,         // 每轮空闲看门狗
}));

// 工具两种接法：JS 闭包（loop 直接调用）……
agent.registerTool("echo", "returns its input",
  JSON.stringify({ type: "object" }),
  async (argsJson) => JSON.stringify({ text: JSON.parse(argsJson).input ?? "" }));

// ……或 pump 占位（callback 传 null）：loop 发出 tool_execution_start，
// 宿主择机用 LLM 分配的 id 回答：
agent.registerTool("askUser", "ask the human",
  JSON.stringify({ type: "object" }), null);

agent.prompt("hello");
const tick = setInterval(() => {
  for (const line of agent.poll()) {
    const ev = JSON.parse(line);
    if (ev.type === "text_delta") process.stdout.write(ev.text);
    if (ev.type === "tool_execution_start") {
      // pump：宿主准备好后回答
      agent.resolveTool(ev.tool_use_id, JSON.stringify({ text: "human says hi" }));
    }
    if (ev.type === "agent_end") { clearInterval(tick); console.log("\n[done]"); }
  }
}, 50);

// —— M3：工具审批门 ——
// manual 工具在执行前暂停；宿主 approveToolCall(id, allow) 决定。
// deny 把 denied_by_host failure 返回给 LLM（模型可据此调整），
// 不是静默失败。与 pump 正交（先审批，放行后进 pump 等待）。
agent.registerToolWithOptions("rmFile", "delete a file",
  JSON.stringify({ type: "object" }),
  async (argsJson) => JSON.stringify({ text: "deleted" }),
  JSON.stringify({ approval: "manual" }));
// 事件循环里：
//   if (ev.type === "tool_execution_start" && ev.tool_name === "rmFile") {
//     agent.approveToolCall(ev.tool_use_id, confirm("allow?"));
//   }

// —— M2：成本快照 ——
// token 累计（不乘价格——宿主拿原始 token 自己算钱）；clearSession 不重置
// {"totalInputTokens":…,"totalOutputTokens":…,"totalCacheReadTokens":…,
//  "totalCacheWriteTokens":…,"totalReasoningTokens":…,"providerCalls":…}
const cost = JSON.parse(agent.costSnapshot());

// 会话是门面持有的 JSON，由宿主负责持久化：
const saved = agent.exportSession();          // {"v":1,"history":[{role,text},…]}
// 之后 / 重载后：
const agent2 = new EmbeddedAgent(JSON.stringify({ provider: "openai", /* … */ }));
agent2.importSession(saved);
agent.clearSession();                          // 清空；会杀死在途写回
```

多轮语义：`prompt()` 发送 `history + [user msg]`；run 结束
（`agent_end`）时把本轮新消息合并回历史。运行中再调 `prompt()`
会排队一个 `busy` 错误，而不是并发竞争。provider 出 `error` 时，
loop 仍会紧随其后以 `agent_end` 收尾（内核契约）——半轮内容不会
被合并（"宁可丢半轮，不可脏历史"）。

## 在线 demo（GitHub Pages）

推到 GitHub 后启用 Pages（Settings → Pages → Source 选 **GitHub Actions**），
`.github/workflows/pages.yml` 会在每次 main 变更时构建 wasm 并部署到：

    https://<user>.github.io/senza-wasm/demo/  （入口页，选 demo 玩）

纯静态（HTML + JS + wasm），无后端；API key 只在访客自己的浏览器
内存里，不经过任何服务器。本地跑同款：`wasm-pack build --target
web --out-dir pkg-web && python3 -m http.server` 后开 `demo/index.html`。

现有四个 demo：**chat**（多轮对话 + 审批门）、**vale**（agent 改写游戏世界）、
**deck**（对话生成 HTML PPT，实时预览 + 下载自包含文件）、
**undercover**（谁是卧底：多 agent 同场、独立 session、信息物理隔离）。

## 测试门面（mock provider）

`provider: "mock"` 接受 `mockScript` 数组（仅 constructor JSON，
不属于公开 API 面）：每项为 `{kind:"text", text}`、
`{kind:"tool_use", toolUseId, name, args}` 或
`{kind:"rate_limit_error"}`。按顺序消耗；耗尽后 mock 回退为
普通文本响应。

```js
const agent = new EmbeddedAgent(JSON.stringify({
  provider: "mock", model: "mock-model",
  mockScript: [
    { kind: "tool_use", toolUseId: "t1", name: "echo", args: "{}" },
    { kind: "text", text: "all done" },
  ],
}));
```

## 架构说明

- **网络留在 Rust 侧**（spec 决策 3）：一套 Rust 实现服务所有宿主；宿主只需提供 `fetch` 全局（浏览器：内置；Node ≥18：内置；Puerts：由 C# 注入，后续 spec）。
- **Pump 模型**：工具要么作为 JS 闭包运行（`registerTool` 传 callback），要么作为 pump 占位（callback 传 `null`）：loop 发出 `tool_execution_start` 事件，宿主以 `resolveTool(toolUseId, resultJson)` 回答——无需跨边界回调。寻址用 LLM 分配的 `tool_use_id`（与 Python SDK 同字段，spec §3.4 已结案）。
- **事件线格式**：带 `type` 标签的 JSON 行（`text_delta`、`tool_call_end`、`agent_end`……），与 Python SDK 的事件字典名保持 1:1，封装层两侧对称。
- **内核零绑定**：所有 `#[wasm_bindgen]` 都在本 crate；内核 crate 以 git rev 锁定，不携带 wasm-bindgen 依赖。
- **maxTurns 是门面守卫**：内核 `agent_loop` 本身没有轮数上限（loop-safety 在 strategy 层，M4）；门面统计 `turn_start` 事件数，超限即通过 cancel token 中止，并发出 `error_type: "resource_limit"`。

## 仓库结构

```
src/lib.rs          # wasm_bindgen 引导（panic hook）+ version()
src/agent.rs        # EmbeddedAgent 门面（prompt/poll/cancel/resolveTool/会话）
src/events.rs       # AgentEvent → JSON 行（17 个类型化变体 + unknown 兜底）
src/tools.rs        # JsTool（JS 闭包）+ PendingTool（pump，按 tool_use_id 寻址）
tests/node_smoke.mjs       # Node 冒烟：完整 mock 轮次、事件序列断言
tests/node_multi_turn.mjs  # 多轮：mock 脚本、历史续接、
                           # pump + 闭包工具、错误路径（进 CI）
tests/node_deck.mjs        # deck demo 冒烟：Deck 模型、工具大纲回显、文档组装器（进 CI）
tests/node_undercover.mjs  # 谁是卧底 demo 冒烟：词库/票型/胜负/状态机、
                           # speak/vote 工具多轮 session（进 CI）
tests/browser_smoke.html   # 浏览器手动门（web 目标）
docs/superpowers/          # spec + 实施计划
npm/                       # 封装层 + 手写 .d.ts（M2）
```
