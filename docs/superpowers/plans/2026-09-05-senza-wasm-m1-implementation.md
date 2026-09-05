# M1 Session Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `EmbeddedAgent` conversation continuity (history continuation, export/import/clear) and a JS-configurable mock script, fixing the verified prompt-text-injection bug.

**Architecture:** All changes in the facade (`src/`): `AgentState` gains a `history` + generation counter; `prompt()` injects the user message into `ctx.messages` (kernel `agent_loop` never reads `config.run.initial_messages` — harness-layer responsibility); AgentEnd's incremental `new_messages` merges back via generation-checked writeback. Mock script maps a serde-tagged enum onto kernel `MockResponse` presets. Session JSON is a facade-owned `{v, history}` contract with a manual stable projection (role + text), distinct from the kernel's serde format.

**Tech Stack:** Rust → wasm32 (wasm-bindgen), kernel crates pinned at rev `358af9f` (features `test-utils`), Node ≥18 smoke tests, wasm-bindgen-test.

**Spec:** `docs/superpowers/specs/2026-09-05-senza-wasm-session-core-design.md`

## Global Constraints

- Kernel crates stay binding-free; all `#[wasm_bindgen]` in this repo (原 spec §2 决策 4)
- Kernel pinned by git rev; NO path dependencies (workspace feature unification pollutes wasm — 2026-09-04 实测)
- Event `type` strings must match Senza Python SDK verbatim (event field `tool_use_id` confirmed identical at Senza `src/shared/event_stream.rs:199` vs our `src/events.rs:121`)
- Every command below runs on `wasm32-unknown-unknown`; test commands:
  - wasm tests: `cargo test --target wasm32-unknown-unknown` (uses wasm-bindgen-test-runner for `#[wasm_bindgen_test]`)
  - clippy: `cargo clippy --target wasm32-unknown-unknown -- -D warnings`
  - fmt: `cargo fmt --check`
  - node smoke: build first with `wasm-pack build --target nodejs --out-dir pkg-node`, then `node tests/node_smoke.mjs`
- Browser-only keys are dev/demo; no key handling added in M1

---

### Task 1: Mock script option

**Files:**
- Modify: `src/agent.rs` (AgentOpts struct ~line 63-81, `new()` mock arm ~line 117-123)
- Test: `tests/node_multi_turn.mjs` (created in this task; extended in later tasks)

**Interfaces:**
- Consumes: kernel `llm_harness_loop::test_utils::{MockLlmClient, MockResponse}` (already imported in `new()` mock arm)
- Produces: `AgentOpts.mock_script: Option<Vec<MockSpec>>` where `MockSpec` is:
  `#[serde(tag = "kind", rename_all = "snake_case")] enum MockSpec { Text { text: String }, ToolUse { tool_use_id: String, name: String, args: String }, RateLimitError }`
  and the constructor accepts `"mockScript"` (serde rename) in options JSON. No new wasm_bindgen exports.

- [ ] **Step 1: Write the failing test**

Create `tests/node_multi_turn.mjs`:

```js
import assert from "node:assert";
import { EmbeddedAgent } from "../pkg-node/senza_wasm.js";

// Two text responses: second prompt must see a DIFFERENT response,
// proving the script is consumed in order.
const agent = new EmbeddedAgent(JSON.stringify({
  provider: "mock",
  model: "mock-model",
  mockScript: [
    { kind: "text", text: "first" },
    { kind: "text", text: "second" },
  ],
}));

async function runTo(agent, needle) {
  const lines = [];
  for (let i = 0; i < 200; i++) {
    await new Promise((r) => setTimeout(r, 25));
    lines.push(...agent.poll());
    if (lines.some((l) => l.includes(needle))) return lines;
  }
  throw new Error("timeout waiting for " + needle + ":\n" + lines.join("\n"));
}

let lines = await runTo(agent, '"type":"agent_end"');
assert(lines.some((l) => l.includes('"text":"first"')), "first script item:\n" + lines.join("\n"));

agent.prompt("again");
lines = await runTo(agent, '"type":"agent_end"');
assert(lines.some((l) => l.includes('"text":"second"')), "second script item:\n" + lines.join("\n"));
console.log("MOCK SCRIPT PASS");
```

- [ ] **Step 2: Run test to verify it fails**

Run: `wasm-pack build --target nodejs --out-dir pkg-node && node tests/node_multi_turn.mjs`
Expected: FAIL — unknown field `mockScript` error from serde ("invalid createAgent options: unknown field `mockScript`")

- [ ] **Step 3: Write minimal implementation**

In `src/agent.rs`, add above `AgentOpts`:

```rust
/// One preset in a mock conversation script (constructor JSON only;
/// not part of the public API surface).
#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MockSpec {
    Text { text: String },
    ToolUse { tool_use_id: String, name: String, args: String },
    RateLimitError,
}
```

In `AgentOpts`, add (with `#[serde(default, rename = "mockScript")]`):

```rust
    #[serde(default, rename = "mockScript")]
    mock_script: Option<Vec<MockSpec>>,
```

In `new()`, replace the mock arm:

```rust
            "mock" => {
                let responses = opts
                    .mock_script
                    .take()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|spec| match spec {
                        MockSpec::Text { text } => {
                            llm_harness_loop::test_utils::MockResponse::text(&text)
                        }
                        MockSpec::ToolUse { tool_use_id, name, args } => {
                            llm_harness_loop::test_utils::MockResponse::tool_use(
                                &tool_use_id, &name, &args,
                            )
                        }
                        MockSpec::RateLimitError => {
                            llm_harness_loop::test_utils::MockResponse::rate_limit_error()
                        }
                    })
                    .collect::<Vec<_>>();
                let client = Arc::new(llm_harness_loop::test_utils::MockLlmClient::new(responses));
                Ok(Self::new_with_client(client, opts))
            }
```

Note: `opts.mock_script.take()` requires `opts` to be `mut` — change the binding to `let mut opts: AgentOpts = ...` if not already.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo clippy --target wasm32-unknown-unknown -- -D warnings && wasm-pack build --target nodejs --out-dir pkg-node && node tests/node_multi_turn.mjs`
Expected: PASS (`MOCK SCRIPT PASS`), clippy clean

- [ ] **Step 5: Verify existing smoke unchanged**

Run: `node tests/node_smoke.mjs`
Expected: PASS — no script defaults to single "mock response"

- [ ] **Step 6: Commit**

```bash
git add src/agent.rs tests/node_multi_turn.mjs
git commit -m "feat: mockScript option — JS-configurable mock response sequences"
```

---

### Task 2: History continuation + prompt injection fix (core bug fix)

**Files:**
- Modify: `src/agent.rs` (AgentState ~line 24-44, `prompt()` ~line 179-233)

**Interfaces:**
- Consumes: kernel `AgentEvent::AgentEnd { new_messages: Vec<AgentMessage> }` (incremental — run-generated messages only, excludes the initial user message; verified loop_fn.rs:264/618)
- Produces: `AgentState.history: Vec<AgentMessage>`, `AgentState.generation: u64`; on AgentEnd: `history = history + [user_msg] + new_messages` (generation-checked). Prompt text now reaches the LLM. Later tasks (session export) read `AgentState.history`.

- [ ] **Step 1: Write the failing test**

Append to `tests/node_multi_turn.mjs`:

```js
// --- history continuation: second request must carry first turn's messages ---
// Without continuation both prompts send exactly 1 message and the mock
// script (2 entries) is consumed by prompt 1 alone.
const agent2 = new EmbeddedAgent(JSON.stringify({
  provider: "mock",
  model: "mock-model",
  mockScript: [
    { kind: "text", text: "answer-one" },
    { kind: "text", text: "answer-two" },
  ],
}));
agent2.prompt("question-one");
await runTo(agent2, '"type":"agent_end"');
agent2.prompt("question-two");
lines = await runTo(agent2, '"type":"agent_end"');
assert(lines.some((l) => l.includes('"text":"answer-two"')),
  "script order broken — history not continued:\n" + lines.join("\n"));
console.log("HISTORY CONTINUATION PASS");
```

Note: this asserts observable behavior (script advanced exactly once per prompt; without history merge the second prompt would consume... actually with current broken code the mock ignores request content entirely, so a true behavioral pin needs the request contents). Extend the assertion below in Task 4 with `captured_requests` via a wasm-bindgen-test (kernel `MockLlmClient.captured_requests` is not reachable from JS; the wasm test asserts request contents directly).

- [ ] **Step 2: Run test to verify it passes for the wrong reason / fails semantics**

Run: `node tests/node_multi_turn.mjs`
Expected: likely PASSES (mock ignores request content; script advances per chat_stream call) — this test pins the *event flow*, the request-content pin comes in Task 4. Record result honestly.

- [ ] **Step 3: Implement history + generation + busy guard**

In `AgentState`, add fields:

```rust
    /// Full conversation history: everything the next prompt must carry.
    /// history + [user_msg of current run] is what AgentContext carries.
    pub history: Vec<AgentMessage>,
    /// Bumped on every prompt/clear; writeback only if generation matches.
    pub generation: u64,
```

(Add `AgentMessage` to the `llm_harness_types` use list; initialize both in `AgentState::new()`.)

Replace `prompt()` body with:

```rust
    pub fn prompt(&self, text: String) {
        let (client, model, max_tokens, temperature, system_prompt, max_turns) = {
            let d = &self.deps;
            (d.client.clone(), d.model.clone(), d.max_tokens, d.temperature, d.system_prompt.clone(), d.max_turns)
        };
        let state = self.state.clone();
        let mut guard = state.lock();
        if guard.running {
            guard.queue.push(
                serde_json::json!({
                    "type": "error",
                    "message": "prompt() called while a run is in flight",
                    "error_type": "busy",
                })
                .to_string(),
            );
            return;
        }
        let user_msg = llm_harness_types::AgentMessage::User(llm_harness_types::UserMessage {
            content: vec![llm_harness_types::ContentBlock::Text { text }],
            timestamp: chrono::Utc::now(),
        });
        let mut history = guard.history.clone();
        history.push(user_msg.clone());
        guard.generation += 1;
        let generation = guard.generation;
        guard.running = true;
        let tools = guard.tools.clone();
        let abort = guard.abort.clone();
        drop(guard);

        wasm_bindgen_futures::spawn_local(async move {
            let request = RunRequest::from_text(""); // metadata only; loop ignores it
            let ctx = AgentContext {
                system_prompt,
                messages: history, // history + [user_msg] — the injection fix
            };
            let config = build_config(&request, &model, max_tokens, temperature, tools, abort.clone());
            let stream = agent_loop(client, ctx, config);
            futures::pin_mut!(stream);
            let mut turns_started = 0u32;
            let mut merged = vec![user_msg];
            while let Some(event) = stream.next().await {
                if matches!(event, AgentEvent::TurnStart { .. }) {
                    turns_started += 1;
                    if turns_started > max_turns {
                        state.lock().queue.push(
                            serde_json::json!({
                                "type": "error",
                                "message": format!("max_turns ({max_turns}) exceeded; aborting"),
                                "error_type": "resource_limit",
                            })
                            .to_string(),
                        );
                        abort.cancel();
                    }
                }
                match &event {
                    AgentEvent::AgentEnd { new_messages } => {
                        merged.extend(new_messages.iter().cloned());
                    }
                    AgentEvent::Error(_) => {
                        // Error does not terminate consumption: kernel contract
                        // guarantees AgentEnd arrives right after (types/events.rs:96).
                        // Half-finished run content: kernel never emits it, so
                        // `merged` stays clean ("宁可丢半轮").
                    }
                    _ => {}
                }
                let done = matches!(event, AgentEvent::AgentEnd { .. });
                state.lock().queue.push(event_to_json(&event));
                if done {
                    break;
                }
            }
            let mut st = state.lock();
            st.running = false;
            if st.generation == generation {
                st.history = merged;
            }
            // generation mismatch: clearSession() happened mid-run — discard.
        });
    }
```

**Refactor folded into this step (prerequisite for Task 4):** extract
options parsing into a helper and an injectable-client constructor:

```rust
fn parse_opts(opts_json: String) -> Result<AgentOpts, JsValue> {
    serde_json::from_str(&opts_json)
        .map_err(|e| JsValue::from_str(&format!("invalid createAgent options: {e}")))
}

impl EmbeddedAgent {
    fn new_with_client_and_opts(
        client: Arc<dyn llm_adapter::provider::Provider>,
        opts: AgentOpts,
    ) -> Self {
        Self::new_with_client(client, opts) // existing private constructor
    }
}
```

`new()` becomes: `let mut opts = parse_opts(opts_json)?; match opts.provider.as_str() { ... }`
(same arms as today). Same-file tests and later tasks construct agents with
an injected `MockLlmClient` via `EmbeddedAgent::new_with_client_and_opts(...)`.

(Keep the existing max_turns guard behavior exactly; the loop above integrates it.)

- [ ] **Step 4: Run all tests**

Run: `cargo clippy --target wasm32-unknown-unknown -- -D warnings && cargo test --target wasm32-unknown-unknown && wasm-pack build --target nodejs --out-dir pkg-node && node tests/node_multi_turn.mjs && node tests/node_smoke.mjs`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add src/agent.rs tests/node_multi_turn.mjs
git commit -m "fix: inject prompt text into conversation; history continuation + busy guard"
```

---

### Task 3: Session export/import/clear

**Files:**
- Modify: `src/agent.rs` (add `#[wasm_bindgen]` methods to the impl block ~line 109-265)
- Test: in-file `#[cfg(test)]` wasm-bindgen-test additions (src/agent.rs bottom) + node test extension

**Interfaces:**
- Consumes: `AgentState.history: Vec<AgentMessage>`, `AgentState.generation: u64` (Task 2)
- Produces (wasm_bindgen exports):
  - `exportSession() -> String` — `{"v":1,"history":[{"role":"user","text":...},...]}`
  - `importSession(json: String) -> Result<(), JsValue>` — replaces history; bumps generation; errors on parse/version mismatch
  - `clearSession()` — empties history; bumps generation (kills in-flight writeback)
- Reuses the existing projection `message_to_value` shape (`{role, text}`) from `src/events.rs` for the stable facade contract: export uses the SAME `message_to_value`, import reconstructs only `user` / `assistant` roles (text-only), erroring on `tool_result`/`branch_summary`/`compaction_summary`/`custom`/`unknown` roles with a clear message. Rationale: sessions at rest only need human-readable turns; tool_result blocks are run-internal and re-derived next run (spec §3.2: 未知块类型打平为 text 兜底 — applied here as: import accepts only what export of a clean conversation produces).

- [ ] **Step 1: Write the failing wasm test**

Add to the `#[cfg(test)] mod tests` in `src/agent.rs` (file bottom; module already has `use super::*`):

```rust
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn session_export_import_roundtrip() {
        let agent = EmbeddedAgent::new(r#"{"provider":"mock","model":"m"}"#.into()).unwrap();
        agent.prompt("hello");
        // Drain until agent_end (spawn_local needs the JS microtask queue,
        // which the async test fn services via await).
        for _ in 0..200 {
            gloo_timers::future::TimeoutFuture::new(25).await;
            if agent.poll().iter().any(|l| l.contains(r#""type":"agent_end""#)) {
                break;
            }
        }
        let exported = agent.export_session();
        let v: serde_json::Value = serde_json::from_str(&exported).unwrap();
        assert_eq!(v["v"], 1);
        let h = v["history"].as_array().unwrap();
        assert_eq!(h.len(), 2, "user + assistant after one turn");
        assert_eq!(h[0]["role"], "user");
        assert_eq!(h[0]["text"], "hello");
        assert_eq!(h[1]["role"], "assistant");

        let agent2 = EmbeddedAgent::new(r#"{"provider":"mock","model":"m"}"#.into()).unwrap();
        agent2.import_session(exported.clone()).unwrap();
        assert_eq!(agent2.export_session(), exported, "roundtrip");
    }
```

(If `gloo-timers` is not a dev-dependency, add it under
`[dev-dependencies]`: `gloo-timers = { version = "0.3", features = ["futures"] }`
— or use `wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(...))`
with a `setTimeout`; pick gloo-timers for brevity.)

Add second test:

```rust
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn session_import_rejects_bad_version() {
        let agent = EmbeddedAgent::new(r#"{"provider":"mock","model":"m"}"#.into()).unwrap();
        let err = agent.import_session(r#"{"v":99,"history":[]}"#.into()).unwrap_err();
        assert!(format!("{err:?}").contains("unsupported session version"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --target wasm32-unknown-unknown`
Expected: FAIL — `export_session` / `import_session` not found

- [ ] **Step 3: Implement the three exports**

First, hoist `message_to_value` visibility in `src/events.rs`: change `fn message_to_value` to `pub(crate) fn message_to_value`.

In `src/agent.rs` `#[wasm_bindgen] impl EmbeddedAgent`, add:

```rust
    /// Export conversation history as facade-owned JSON: `{"v":1,"history":[...]}`.
    /// Host persists this (localStorage / IndexedDB / file); import restores.
    pub fn export_session(&self) -> String {
        let state = self.state.lock();
        let history: Vec<serde_json::Value> = state
            .history
            .iter()
            .map(crate::events::message_to_value)
            .collect();
        serde_json::json!({ "v": 1, "history": history }).to_string()
    }

    /// Restore a session previously produced by export_session.
    /// Replaces history; bumps generation (discards any in-flight writeback).
    pub fn import_session(&self, json: String) -> Result<(), JsValue> {
        let v: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| JsValue::from_str(&format!("invalid session json: {e}")))?;
        if v.get("v").and_then(|x| x.as_i64()) != Some(1) {
            return Err(JsValue::from_str("unsupported session version (expected 1)"));
        }
        let entries = v
            .get("history")
            .and_then(|x| x.as_array())
            .ok_or_else(|| JsValue::from_str("session json missing history array"))?;
        let mut history = Vec::with_capacity(entries.len());
        for e in entries {
            let role = e.get("role").and_then(|x| x.as_str()).unwrap_or("unknown");
            let text = e.get("text").and_then(|x| x.as_str()).unwrap_or("");
            history.push(match role {
                "user" => llm_harness_types::AgentMessage::User(llm_harness_types::UserMessage {
                    content: vec![llm_harness_types::ContentBlock::Text { text: text.into() }],
                    timestamp: chrono::Utc::now(),
                }),
                "assistant" => llm_harness_types::AgentMessage::Assistant(
                    llm_harness_types::AssistantMessage {
                        kind: llm_harness_types::AssistantMessageKind::FinalAnswer,
                        message_id: String::new(),
                        turn_id: String::new(),
                        content: vec![llm_harness_types::ContentBlock::Text { text: text.into() }],
                        stop_reason: None,
                        timestamp: chrono::Utc::now(),
                        provider: None,
                        api: None,
                        model: None,
                        usage: None,
                        error_message: None,
                    },
                ),
                other => {
                    return Err(JsValue::from_str(&format!(
                        "session entry role '{other}' is not importable (run-internal)"
                    )));
                }
            });
        }
        let mut state = self.state.lock();
        state.history = history;
        state.generation += 1;
        Ok(())
    }

    /// Clear history. Does not interrupt a running run; the in-flight
    /// writeback is discarded via the generation counter.
    pub fn clear_session(&self) {
        let mut state = self.state.lock();
        state.history.clear();
        state.generation += 1;
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test --target wasm32-unknown-unknown && cargo clippy --target wasm32-unknown-unknown -- -D warnings && cargo fmt && wasm-pack build --target nodejs --out-dir pkg-node && node tests/node_multi_turn.mjs`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add src/agent.rs src/events.rs
git commit -m "feat: exportSession/importSession/clearSession — facade-owned session JSON (v1)"
```

---

### Task 4: Request-content pin (wasm test asserting kernel receives history)

**Files:**
- Test: `src/agent.rs` `#[cfg(test)]` additions

**Interfaces:**
- Consumes: kernel `MockLlmClient.captured_requests: parking_lot-style Mutex<Vec<ChatRequest>>` (public field, rev `358af9f`); the Task 2 refactor `parse_opts(json) -> Result<AgentOpts, JsValue>` + same-file access to the private `new_with_client` constructor
- Produces: the direct kernel-side regression pin for the prompt-injection bug — request #2's message list is strictly longer than request #1's

- [ ] **Step 1: Write the test (Task 2 made it pass; this pins it)**

Add to `src/agent.rs` tests module:

```rust
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn second_request_carries_first_turn_history() {
        use llm_harness_loop::test_utils::{MockLlmClient, MockResponse};
        let client = Arc::new(MockLlmClient::new(vec![
            MockResponse::text("a"),
            MockResponse::text("b"),
        ]));
        let provider: Arc<dyn llm_adapter::provider::Provider> = client.clone();
        let agent = EmbeddedAgent::new_with_client(
            provider,
            parse_opts(r#"{"provider":"mock","model":"m"}"#.into()).unwrap(),
        );
        agent.prompt("one");
        for _ in 0..200 {
            gloo_timers::future::TimeoutFuture::new(25).await;
            if agent.poll().iter().any(|l| l.contains(r#""type":"agent_end""#)) {
                break;
            }
        }
        agent.prompt("two");
        for _ in 0..200 {
            gloo_timers::future::TimeoutFuture::new(25).await;
            if agent.poll().iter().any(|l| l.contains(r#""type":"agent_end""#)) {
                break;
            }
        }
        let reqs = client.captured_requests.lock();
        assert_eq!(reqs.len(), 2, "two chat_stream calls");
        let count = |r: &llm_adapter::types::ChatRequest| r.messages().len();
        assert!(
            count(&reqs[1]) > count(&reqs[0]),
            "second request must carry first-turn history ({} vs {})",
            count(&reqs[0]),
            count(&reqs[1]),
        );
    }
```

(If `ChatRequest` exposes `messages()` differently, check
`llm_adapter::types::ChatRequest` in the rev `358af9f` checkout of
llm-api-adapter — the field access is `req.messages()` per
loop_fn.rs:3153 `req.messages()`.)

- [ ] **Step 2: Run to verify it fails (would fail pre-Task-2)**

Run: `cargo test --target wasm32-unknown-unknown`
Expected: PASS post-Task-2 (this is the regression pin for the injection bug). To prove it pins the bug: `git stash` the Task 2 history change is impractical — accept the pin as-is; the captured_requests length assertions fail on pre-fix code by construction (request #2 would have equal message count).

- [ ] **Step 3: Run full suite**

Run: `cargo test --target wasm32-unknown-unknown && wasm-pack build --target nodejs --out-dir pkg-node && node tests/node_multi_turn.mjs && node tests/node_smoke.mjs`
Expected: all PASS

- [ ] **Step 4: Commit**

```bash
git add src/agent.rs
git commit -m "test: pin history continuation via MockLlmClient.captured_requests"
```

---

### Task 5: Multi-turn tool conversation smoke (pump + JsTool paths + error paths)

**Files:**
- Test: `tests/node_multi_turn.mjs` (extend)

**Interfaces:**
- Consumes: mock script `tool_use` (Task 1), history continuation (Task 2), `resolve_tool` (existing, `tool_use_id` addressing — confirmed identical to Python SDK event field name)
- Produces: CI-run smoke proving full multi-turn tool loop

- [ ] **Step 1: Write the pump-path test**

Append to `tests/node_multi_turn.mjs`:

```js
// --- multi-turn tool conversation, pump path ---
const agent3 = new EmbeddedAgent(JSON.stringify({
  provider: "mock",
  model: "mock-model",
  mockScript: [
    { kind: "tool_use", toolUseId: "t1", name: "echo", args: "{\"input\":\"hi\"}" },
    { kind: "text", text: "tool done" },
  ],
}));
agent3.registerTool("echo", "returns its input", JSON.stringify({ type: "object" }), null);

agent3.prompt("use the tool");
// First run ends when tool_execution_start appears (pump tool blocks).
let seq = [];
for (let i = 0; i < 200; i++) {
  await new Promise((r) => setTimeout(r, 25));
  for (const l of agent3.poll()) {
    const ev = JSON.parse(l);
    seq.push(ev.type);
    if (ev.type === "tool_execution_start") {
      assert.strictEqual(ev.tool_use_id, "t1", "pump addressing field is tool_use_id");
      agent3.resolveTool(ev.tool_use_id, JSON.stringify({ text: "echoed:hi" }));
    }
  }
  if (seq.includes("agent_end")) break;
}
assert(seq.includes("tool_execution_start"), "pump path surfaced tool call:\n" + seq.join(" -> "));
assert(seq.includes("text_delta"), "second turn produced text:\n" + seq.join(" -> "));
assert(seq.includes("agent_end"), "run completed:\n" + seq.join(" -> "));
const t1 = seq.indexOf("tool_execution_start");
const t2 = seq.indexOf("turn_start", t1);
assert(t2 > t1, "resolveTool advanced to a second turn:\n" + seq.join(" -> "));
console.log("MULTI-TURN TOOL (pump) PASS");
```

- [ ] **Step 2: Run to verify**

Run: `node tests/node_multi_turn.mjs`
Expected: PASS (may already pass post-Task-2 — this is the spec §4 acceptance matrix entry; if it fails, fix Task 2 merge logic before proceeding)

- [ ] **Step 3: Write the JsTool-closure-path test**

Append:

```js
// --- multi-turn tool conversation, JS-closure path (auto-resolve) ---
const agent4 = new EmbeddedAgent(JSON.stringify({
  provider: "mock",
  model: "mock-model",
  mockScript: [
    { kind: "tool_use", toolUseId: "t1", name: "echo", args: "{\"input\":\"hi\"}" },
    { kind: "text", text: "closure done" },
  ],
}));
agent4.registerTool("echo", "returns its input", JSON.stringify({ type: "object" }),
  async (argsJson) => JSON.stringify({ text: "echoed:" + JSON.parse(argsJson).input }));

agent4.prompt("use the tool");
const seq4 = [];
for (let i = 0; i < 200; i++) {
  await new Promise((r) => setTimeout(r, 25));
  for (const l of agent4.poll()) seq4.push(JSON.parse(l).type);
  if (seq4.includes("agent_end")) break;
}
assert(seq4.includes("tool_execution_start") && seq4.includes("text_delta") && seq4.includes("agent_end"),
  "closure tool auto-completed two turns:\n" + seq4.join(" -> "));
console.log("MULTI-TURN TOOL (closure) PASS");
```

- [ ] **Step 4: Write the error-path tests**

Append:

```js
// --- error paths ---
// resolveTool with unknown id → JsValue error
const agent5 = new EmbeddedAgent(JSON.stringify({ provider: "mock", model: "mock-model" }));
assert.throws(() => agent5.resolveTool("nonexistent-id", "{}"),
  /unknown or already-resolved tool_use_id/);

// max_turns guard: script of endless tool_use + maxTurns=2 → resource_limit error
const agent6 = new EmbeddedAgent(JSON.stringify({
  provider: "mock",
  model: "mock-model",
  maxTurns: 2,
  mockScript: [
    { kind: "tool_use", toolUseId: "t1", name: "echo", args: "{}" },
    { kind: "tool_use", toolUseId: "t2", name: "echo", args: "{}" },
    { kind: "tool_use", toolUseId: "t3", name: "echo", args: "{}" },
  ],
}));
agent6.registerTool("echo", "returns its input", JSON.stringify({ type: "object" }),
  async () => JSON.stringify({ text: "ok" }));
agent6.prompt("go");
let sawLimit = false;
for (let i = 0; i < 200 && !sawLimit; i++) {
  await new Promise((r) => setTimeout(r, 25));
  for (const l of agent6.poll()) {
    const ev = JSON.parse(l);
    if (ev.type === "error" && ev.error_type === "resource_limit") sawLimit = true;
  }
}
assert(sawLimit, "max_turns guard fired:\n" + "no resource_limit error seen");
console.log("ERROR PATHS PASS");
```

- [ ] **Step 5: Run everything**

Run: `cargo fmt --check && cargo clippy --target wasm32-unknown-unknown -- -D warnings && cargo test --target wasm32-unknown-unknown && wasm-pack build --target nodejs --out-dir pkg-node && node tests/node_multi_turn.mjs && node tests/node_smoke.mjs`
Expected: all PASS

- [ ] **Step 6: Add node_multi_turn to CI**

Modify `.github/workflows/ci.yml` `test-node` job, after the existing smoke line:

```yaml
      - run: node tests/node_multi_turn.mjs
```

- [ ] **Step 7: Commit**

```bash
git add tests/node_multi_turn.mjs .github/workflows/ci.yml
git commit -m "test: multi-turn tool smoke (pump + closure paths) + error paths; CI"
```

---

### Task 6: Browser smoke (manual gate, not CI)

**Files:**
- Create: `tests/browser_smoke.html`

**Interfaces:**
- Consumes: `pkg-bundler/` build output (built on demand, gitignored)
- Produces: self-checking page; PASS/FAIL string in DOM `#result` and console

- [ ] **Step 1: Write the page**

Create `tests/browser_smoke.html` — bundler-target module import, same assertions as the pump-path multi-turn test (Task 5 Step 1), condensed:

```html
<!doctype html>
<html>
<head><meta charset="utf-8"><title>senza-wasm browser smoke</title></head>
<body>
<pre id="result">RUNNING...</pre>
<script type="module">
const result = document.getElementById("result");
const fail = (msg) => { result.textContent = "FAIL: " + msg; console.error(msg); };
try {
  const { EmbeddedAgent } = await import("../pkg-bundler/senza_wasm.js");
  const agent = new EmbeddedAgent(JSON.stringify({
    provider: "mock",
    model: "mock-model",
    mockScript: [
      { kind: "tool_use", toolUseId: "t1", name: "echo", args: "{\"input\":\"hi\"}" },
      { kind: "text", text: "browser done" },
    ],
  }));
  agent.registerTool("echo", "returns its input", JSON.stringify({ type: "object" }), null);
  agent.prompt("use the tool");
  const seq = [];
  for (let i = 0; i < 400; i++) {
    await new Promise((r) => setTimeout(r, 25));
    for (const l of agent.poll()) {
      const ev = JSON.parse(l);
      seq.push(ev.type);
      if (ev.type === "tool_execution_start") agent.resolveTool(ev.tool_use_id, JSON.stringify({ text: "ok" }));
    }
    if (seq.includes("agent_end")) break;
  }
  const need = ["tool_execution_start", "turn_start", "text_delta", "agent_end"];
  for (const t of need) if (!seq.includes(t)) throw new Error("missing " + t + " in: " + seq.join(" -> "));
  result.textContent = "BROWSER SMOKE PASS — " + seq.join(" -> ");
} catch (e) { fail(e.message); }
</script>
</body>
</html>
```

- [ ] **Step 2: Build bundler target and serve**

Run: `wasm-pack build --target bundler --out-dir pkg-bundler && python3 -m http.server 8931 &` (serve repo root; ES module import needs http, not file://)

- [ ] **Step 3: Verify in headless Chromium**

Use the harness browser tool: open `http://localhost:8931/tests/browser_smoke.html`, read `#result` textContent.
Expected: `BROWSER SMOKE PASS — agent_start -> turn_start -> ... -> agent_end`
Kill the http server afterwards.

- [ ] **Step 4: Commit**

```bash
git add tests/browser_smoke.html
git commit -m "test: browser smoke page (manual gate, bundler target)"
```

---

### Task 7: Docs + call_id conclusion + final verification

**Files:**
- Modify: `README.md` (Status section, smoke instructions), `docs/superpowers/specs/2026-09-05-senza-wasm-session-core-design.md` (§3.4 conclusion)

**Interfaces:**
- Consumes: everything above
- Produces: docs reflect reality; spec §3.4 records the Python SDK finding

- [ ] **Step 1: Write the call_id conclusion into spec §3.4**

Replace the §3.4 body with the verified conclusion:

```markdown
**复查结论（2026-09-05，已定）**：Python SDK 只有 callback 工具路径
（`Senza/src/core/pytool.rs` 的 Py callback），**没有 pump 模式**——
不存在可对齐的 call_id 语义。事件 JSON 字段名两侧已一致
（Senza `src/shared/event_stream.rs:199` 与本仓库 `src/events.rs`
均为 `tool_use_id`）。决议：pump 寻址字段保持 `tool_use_id`，
作为 senza-wasm 自有契约；无变更。
```

- [ ] **Step 2: Update README**

In `README.md` Status section, replace the "Work in progress" paragraph:

```markdown
## Status

v0.1 core (agent loop, streaming, tools, mock/openai providers) is done.
M1 adds conversation continuity: `prompt()` carries history, sessions
export/import as JSON (`exportSession`/`importSession`/`clearSession`),
mock provider accepts JS-configured scripts (`mockScript`). See
`ROADMAP.md` and `docs/superpowers/specs/`.
```

And extend the smoke section:

```bash
# multi-turn tool smoke (pump + closure paths + error paths)
wasm-pack build --target nodejs --out-dir pkg-node
node tests/node_multi_turn.mjs
```

- [ ] **Step 3: Full verification suite (the spec §6 acceptance)**

Run: `cargo fmt --check && cargo clippy --target wasm32-unknown-unknown -- -D warnings && cargo test --target wasm32-unknown-unknown && wasm-pack build --target nodejs --out-dir pkg-node && node tests/node_smoke.mjs && node tests/node_multi_turn.mjs`
Expected: all green

- [ ] **Step 4: Commit**

```bash
git add README.md docs/superpowers/specs/2026-09-05-senza-wasm-session-core-design.md
git commit -m "docs: M1 wrap-up — session API in README, call_id conclusion in spec §3.4"
```
