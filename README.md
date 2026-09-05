# senza-wasm

TypeScript binding for the [llm-harness](https://github.com/oh-my-harness/llm-harness-runtime) agent loop: the in-process engine (agent loop, streaming, tools, retry/circuit-breaker) compiled to WebAssembly, consumable from browsers and Node.

```
┌─────────────────────────────┐
│  Your app (TS / browser /   │
│  Node / Puerts)             │
│   createAgent().prompt(...) │
│   poll() → JSON-line events │
└──────────────┬──────────────┘
               │ wasm-bindgen
┌──────────────▼──────────────┐
│  senza-wasm (this repo)     │  ← all #[wasm_bindgen] here
├─────────────────────────────┤
│  llm-harness-loop (Rust)    │  ← pinned git rev, binding-free
│  network stays in Rust      │    (reqwest wasm → fetch)
└─────────────────────────────┘
```

## Status

Work in progress — see `docs` in the design spec (oh-my-harness/Senza, `docs/superpowers/specs/2026-09-03-senza-wasm-design.md`) and the implementation plan (`docs/superpowers/plans/2026-09-04-senza-wasm-implementation.md`).

## Build (local)

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-pack --locked

# dual-target build
wasm-pack build --target bundler --out-dir pkg-bundler
wasm-pack build --target nodejs --out-dir pkg-node

# smoke test (Node)
node tests/node_smoke.mjs
```

## Consume (after first npm release)

```ts
import { createAgent } from "senza-wasm";

const agent = createAgent(JSON.stringify({
  provider: "openai",   // or "mock"
  apiKey: "...",        // browser: dev/demo only — production must proxy through a gateway
  model: "gpt-4o-mini",
  systemPrompt: "You are helpful.",
}));

agent.registerTool("echo", "returns its input",
  JSON.stringify({ type: "object" }),
  async (argsJson) => JSON.stringify({ text: JSON.parse(argsJson).input ?? "" }));

agent.prompt("hello");
const tick = setInterval(() => {
  for (const line of agent.poll()) {
    const ev = JSON.parse(line);
    if (ev.type === "text_delta") process.stdout.write(ev.text);
    if (ev.type === "agent_end") { clearInterval(tick); console.log("\n[done]"); }
  }
}, 50);
```

## Architecture notes

- **Network stays in Rust** (spec decision 3): one Rust implementation serves every host; hosts only need to provide a `fetch` global (browser: built-in; Node ≥18: built-in; Puerts: supplied by C#, later spec).
- **Pump model**: tools either run as JS closures (`registerTool` with a callback) or as pump placeholders (callback = `null`): the loop surfaces `tool_call` events and the host answers with `resolveTool(callId, resultJson)` — no cross-boundary callbacks needed.
- **Event wire format**: JSON lines with a `type` tag (`text_delta`, `tool_call_end`, `agent_end`, …), kept 1:1 with the Python SDK's event dict names so wrapper layers stay symmetric.
- **Kernel stays binding-free**: all `#[wasm_bindgen]` lives in this crate; kernel crates are pinned by git rev and carry no wasm-bindgen dependency.

## Repository layout

```
src/lib.rs          # wasm_bindgen bootstrap + version()
src/agent.rs        # EmbeddedAgent facade (prompt/poll/cancel/resolve_tool)
src/events.rs       # AgentEvent → JSON lines (17 typed variants + unknown catch-all)
src/tools.rs        # JsTool (JS closure) + PendingTool (pump placeholder)
tests/node_smoke.mjs# Node smoke: full mock turn, event-sequence assertions
npm/                # wrapper layer + hand-written .d.ts
```
