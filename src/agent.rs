//! `EmbeddedAgent` — the `#[wasm_bindgen]` facade over `agent_loop`
//! (senza-wasm spec §4.2).
//!
//! Pump model: `prompt()` spawns a local task driving the agent loop,
//! events queue as JSON lines, the host drains them each frame via
//! `poll()`. Tools arrive as JS closures (`JsTool`) or pump placeholders
//! (`PendingTool`) resolved via `resolve_tool`.

use std::sync::Arc;

use futures::StreamExt;
use parking_lot::Mutex;
use wasm_bindgen::prelude::*;

use llm_harness_loop::{DefaultConvertToLlm, FinalAnswerMode, LoopConfig, agent_loop};
use llm_harness_types::{
    AgentContext, AgentEvent, RunContext, RunRequest, StreamOptions, Tool, ToolExecutionMode,
};
use tokio_util::sync::CancellationToken;

use crate::events::event_to_json;
use crate::tools::JsTool;

pub(crate) struct AgentState {
    pub queue: Vec<String>,
    pub abort: CancellationToken,
    pub tools: Vec<Arc<dyn llm_harness_types::Tool>>,
    pub running: bool,
    /// Full conversation history: everything the next prompt must carry.
    /// During a run, `history + [current user msg]` is what the loop sees.
    pub history: Vec<llm_harness_types::AgentMessage>,
    /// Bumped on every prompt/clear/import; history writeback only when
    /// the run's captured generation matches (clearSession mid-run drops
    /// the stale merge).
    pub generation: u64,
    /// Pump tools by name; `resolve_tool` routes via PendingTool::resolve
    /// (keyed by the LLM-assigned tool_use_id, not a facade uuid).
    pub pump_by_name: std::collections::HashMap<String, Arc<crate::tools::PendingTool>>,
}

impl AgentState {
    fn new() -> Self {
        Self {
            queue: Vec::new(),
            abort: CancellationToken::new(),
            tools: Vec::new(),
            running: false,
            history: Vec::new(),
            generation: 0,
            pump_by_name: std::collections::HashMap::new(),
        }
    }
}

/// Immutable per-agent wiring (provider + model parameters).
struct AgentDeps {
    client: Arc<dyn llm_adapter::provider::Provider>,
    model: String,
    max_tokens: u32,
    temperature: Option<f32>,
    system_prompt: Option<String>,
    max_turns: u32,
}

#[wasm_bindgen]
pub struct EmbeddedAgent {
    state: Arc<Mutex<AgentState>>,
    deps: AgentDeps,
}

/// One preset in a mock conversation script (constructor JSON only;
/// not part of the public API surface).
#[derive(serde::Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum MockSpec {
    Text {
        text: String,
    },
    ToolUse {
        tool_use_id: String,
        name: String,
        args: String,
    },
    RateLimitError,
}

/// Options accepted by `createAgent` (JSON string).
#[derive(serde::Deserialize)]
struct AgentOpts {
    /// `mock` (test facade) or `openai`.
    #[serde(default = "default_provider")]
    provider: String,
    #[serde(default, rename = "apiKey")]
    api_key: String,
    #[serde(default, rename = "baseUrl")]
    base_url: Option<String>,
    model: String,
    #[serde(default, rename = "systemPrompt")]
    system_prompt: Option<String>,
    #[serde(default = "default_max_tokens", rename = "maxTokens")]
    max_tokens: u32,
    #[serde(default = "default_max_turns", rename = "maxTurns")]
    max_turns: u32,
    #[serde(default)]
    temperature: Option<f32>,
    /// Mock conversation script (provider "mock" only). Consumed in
    /// order; after exhaustion the mock falls back to plain EndTurn text.
    #[serde(default, rename = "mockScript")]
    mock_script: Option<Vec<MockSpec>>,
}

fn default_provider() -> String {
    "openai".into()
}
fn default_max_tokens() -> u32 {
    4096
}
fn default_max_turns() -> u32 {
    16
}

impl EmbeddedAgent {
    fn new_with_client(client: Arc<dyn llm_adapter::provider::Provider>, opts: AgentOpts) -> Self {
        Self {
            state: Arc::new(Mutex::new(AgentState::new())),
            deps: AgentDeps {
                client,
                model: opts.model,
                max_tokens: opts.max_tokens,
                temperature: opts.temperature,
                system_prompt: opts.system_prompt,
                max_turns: opts.max_turns,
            },
        }
    }
}

/// Parse the constructor options JSON (single parse point; tests and
/// later tasks reuse it with an injected client).
fn parse_opts(opts_json: String) -> Result<AgentOpts, JsValue> {
    serde_json::from_str(&opts_json)
        .map_err(|e| JsValue::from_str(&format!("invalid createAgent options: {e}")))
}

#[wasm_bindgen]
impl EmbeddedAgent {
    /// Create an agent from a JSON options string:
    /// `{provider, apiKey, baseUrl?, model, systemPrompt?, maxTokens?, maxTurns?, temperature?}`.
    #[wasm_bindgen(constructor)]
    pub fn new(opts_json: String) -> Result<EmbeddedAgent, JsValue> {
        let mut opts = parse_opts(opts_json)?;
        match opts.provider.as_str() {
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
                        MockSpec::ToolUse {
                            tool_use_id,
                            name,
                            args,
                        } => llm_harness_loop::test_utils::MockResponse::tool_use(
                            &tool_use_id,
                            &name,
                            &args,
                        ),
                        MockSpec::RateLimitError => {
                            llm_harness_loop::test_utils::MockResponse::rate_limit_error()
                        }
                    })
                    .collect::<Vec<_>>();
                let client = Arc::new(llm_harness_loop::test_utils::MockLlmClient::new(responses));
                Ok(Self::new_with_client(client, opts))
            }
            "openai" => {
                let mut builder = llm_harness_loop::OpenAIProvider::builder(opts.api_key.clone());
                if let Some(url) = &opts.base_url {
                    builder = builder.base_url(url.clone());
                }
                let client: Arc<dyn llm_adapter::provider::Provider> = Arc::new(builder.build());
                Ok(Self::new_with_client(client, opts))
            }
            other => Err(JsValue::from_str(&format!(
                "unknown provider '{other}' (expected 'openai' or 'mock')"
            ))),
        }
    }

    /// Register a tool. With `callback` (an async JS function
    /// `(argsJson) => Promise<resultJson>`) the loop calls it directly;
    /// with `null` the tool becomes a pump placeholder: the loop surfaces
    /// `tool_execution_start` events and the host must call
    /// `resolve_tool(toolUseId, resultJson)`.
    #[wasm_bindgen(js_name = registerTool)]
    pub fn register_tool(
        &self,
        name: String,
        description: String,
        schema_json: String,
        callback: Option<js_sys::Function>,
    ) -> Result<(), JsValue> {
        let schema: serde_json::Value = serde_json::from_str(&schema_json)
            .map_err(|e| JsValue::from_str(&format!("invalid tool schema: {e}")))?;
        let mut state = self.state.lock();
        match callback {
            Some(cb) => {
                state
                    .tools
                    .push(Arc::new(JsTool::new(name, description, schema, cb)));
            }
            None => {
                // Pump placeholder. resolve_tool addresses calls by the
                // LLM-assigned tool_use_id (surfaced on the
                // tool_execution_start event); PendingTool::execute parks
                // a one-shot channel under that id when the loop calls it.
                let tool = crate::tools::PendingTool::new(name, description, schema);
                state
                    .pump_by_name
                    .insert(tool.name().to_string(), tool.clone());
                state.tools.push(tool);
            }
        }
        Ok(())
    }

    /// Start one run with `text` as the user message. Returns immediately;
    /// drain events with `poll()`. The conversation continues: the loop
    /// receives `history + [user msg]` — the kernel's `agent_loop` does NOT
    /// read `config.run.initial_messages` (harness-layer responsibility,
    /// see M1 spec §1), so the caller must inject into ctx.
    pub fn prompt(&self, text: String) {
        let (client, model, max_tokens, temperature, system_prompt, max_turns) = {
            let d = &self.deps;
            (
                d.client.clone(),
                d.model.clone(),
                d.max_tokens,
                d.temperature,
                d.system_prompt.clone(),
                d.max_turns,
            )
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
            // Metadata only; the kernel loop ignores initial_messages.
            let request = RunRequest::default();
            let ctx = AgentContext {
                system_prompt,
                messages: history, // history + [user_msg] — the injection fix
            };
            let config = build_config(
                &request,
                &model,
                max_tokens,
                temperature,
                tools,
                abort.clone(),
            );
            let stream = agent_loop(client, ctx, config);
            futures::pin_mut!(stream);
            let mut turns_started = 0u32;
            // Writeback payload: the user msg + everything the loop emits
            // as new_messages (incremental, excludes the initial user msg —
            // kernel loop_fn.rs:264).
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
                if let AgentEvent::AgentEnd { new_messages } = &event {
                    merged.extend(new_messages.iter().cloned());
                }
                // Error does NOT terminate consumption: the kernel contract
                // guarantees AgentEnd arrives right after (types/events.rs:96).
                // Half-finished content is never emitted on the error path,
                // so `merged` stays clean ("宁可丢半轮，不可脏历史").
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
            // generation mismatch: clear/import happened mid-run — discard.
        });
    }

    /// Drain queued events as JSON lines (one string per event).
    pub fn poll(&self) -> Vec<String> {
        let mut state = self.state.lock();
        std::mem::take(&mut state.queue)
    }

    /// Abort the current run. `error`/`aborted` events follow on `poll()`.
    pub fn cancel(&self) {
        self.state.lock().abort.cancel();
    }

    /// Whether a run is currently in flight.
    #[wasm_bindgen(js_name = isRunning)]
    pub fn is_running(&self) -> bool {
        self.state.lock().running
    }

    /// Resolve a pump tool call. `tool_use_id` is the LLM-assigned id from
    /// the `tool_execution_start` event; routing goes through the
    /// PendingTool registered under the tool's name.
    #[wasm_bindgen(js_name = resolveTool)]
    pub fn resolve_tool(&self, tool_use_id: String, result_json: String) -> Result<(), JsValue> {
        let state = self.state.lock();
        // A pending id lives in exactly one PendingTool; try each.
        for tool in state.pump_by_name.values() {
            if tool.resolve(&tool_use_id, result_json.clone()).is_ok() {
                return Ok(());
            }
        }
        Err(JsValue::from_str(&format!(
            "resolve_tool: unknown or already-resolved tool_use_id '{tool_use_id}'"
        )))
    }

    /// Export conversation history as facade-owned JSON:
    /// `{"v":1,"history":[{"role":..,"text":..},...]}`. The host persists
    /// this (localStorage / IndexedDB / file); `importSession` restores it.
    #[wasm_bindgen(js_name = exportSession)]
    pub fn export_session(&self) -> String {
        let state = self.state.lock();
        let history: Vec<serde_json::Value> = state
            .history
            .iter()
            .map(crate::events::message_to_value)
            .collect();
        serde_json::json!({ "v": 1, "history": history }).to_string()
    }

    /// Restore a session previously produced by `exportSession`.
    /// Replaces history; bumps the generation (discards any in-flight
    /// writeback from a running prompt).
    #[wasm_bindgen(js_name = importSession)]
    pub fn import_session(&self, json: String) -> Result<(), JsValue> {
        let v: serde_json::Value = serde_json::from_str(&json)
            .map_err(|e| JsValue::from_str(&format!("invalid session json: {e}")))?;
        if v.get("v").and_then(|x| x.as_i64()) != Some(1) {
            return Err(JsValue::from_str(
                "unsupported session version (expected 1)",
            ));
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
    #[wasm_bindgen(js_name = clearSession)]
    pub fn clear_session(&self) {
        let mut state = self.state.lock();
        state.history.clear();
        state.generation += 1;
    }
}

/// Single `LoopConfig` construction point — mirrors
/// `llm-harness-runtime/tests/wasm-smoke/src/lib.rs::mock_config`
/// field-for-field, with the idle watchdog armed (exercises the
/// kernel's futures-timer bridge on every turn).
fn build_config(
    request: &RunRequest,
    model: &str,
    max_tokens: u32,
    temperature: Option<f32>,
    tools: Vec<Arc<dyn llm_harness_types::Tool>>,
    abort: CancellationToken,
) -> LoopConfig {
    LoopConfig {
        run: Arc::new(RunContext::new(RunRequest {
            initial_messages: request.initial_messages.clone(),
            extensions: llm_harness_types::RunExtensions::new(),
        })),
        model: model.to_string(),
        max_tokens,
        temperature,
        thinking_level: llm_harness_types::ThinkingLevel::Off,
        tools,
        active_tools: None,
        default_execution_mode: ToolExecutionMode::Parallel,
        final_answer_mode: FinalAnswerMode::default(),
        env: Arc::new(llm_harness_loop::test_utils::NoOpEnv),
        abort,
        stream_options: StreamOptions {
            stream_idle_timeout_ms: Some(5000),
            ..StreamOptions::default()
        },
        convert_to_llm: Arc::new(DefaultConvertToLlm::new()),
        transform_context: None,
        prepare_next_turn: None,
        should_stop: None,
        before_provider_request: None,
        after_provider_response: None,
        final_answer_validator: None,
        provider_error: None,
        steer_rx: None,
        follow_up_rx: None,
        retry: None,
        response_format: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gloo_timers::future::TimeoutFuture;
    use wasm_bindgen_test::wasm_bindgen_test;

    async fn drain_to_end(agent: &EmbeddedAgent) {
        for _ in 0..400 {
            TimeoutFuture::new(25).await;
            if agent
                .poll()
                .iter()
                .any(|l| l.contains(r#""type":"agent_end""#))
            {
                return;
            }
        }
        panic!("timeout waiting for agent_end");
    }

    #[wasm_bindgen_test]
    async fn session_export_import_roundtrip() {
        let agent = EmbeddedAgent::new(r#"{"provider":"mock","model":"m"}"#.into()).unwrap();
        agent.prompt("hello".into());
        drain_to_end(&agent).await;

        let exported = agent.export_session();
        let v: serde_json::Value = serde_json::from_str(&exported).unwrap();
        assert_eq!(v["v"], 1);
        let h = v["history"].as_array().expect("history array");
        assert_eq!(h.len(), 2, "user + assistant after one turn: {exported}");
        assert_eq!(h[0]["role"], "user");
        assert_eq!(h[0]["text"], "hello");
        assert_eq!(h[1]["role"], "assistant");

        let agent2 = EmbeddedAgent::new(r#"{"provider":"mock","model":"m"}"#.into()).unwrap();
        agent2.import_session(exported.clone()).unwrap();
        assert_eq!(agent2.export_session(), exported, "roundtrip");
    }

    #[wasm_bindgen_test]
    async fn session_import_rejects_bad_version() {
        let agent = EmbeddedAgent::new(r#"{"provider":"mock","model":"m"}"#.into()).unwrap();
        let err = agent
            .import_session(r#"{"v":99,"history":[]}"#.into())
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("unsupported session version"),
            "got: {err:?}"
        );
    }

    /// Regression pin for the v0.1 injection bug (M1 spec §1): the kernel
    /// `agent_loop` never reads `config.run.initial_messages`, so the
    /// facade must inject history + [user_msg] into `ctx.messages`. This
    /// asserts the SECOND provider request carries a longer message list
    /// than the first — true only when history continuation works.
    #[wasm_bindgen_test]
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
        agent.prompt("one".into());
        drain_to_end(&agent).await;
        agent.prompt("two".into());
        drain_to_end(&agent).await;

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

    /// maxTurns guard (M1 spec): the facade aborts the run and surfaces a
    /// `resource_limit` error once TurnStart count exceeds the option.
    /// Also pins the camelCase `maxTurns` rename — serde silently ignored
    /// the JS-style key before, leaving the default of 16 in place.
    #[wasm_bindgen_test]
    async fn max_turns_guard_aborts_with_resource_limit() {
        let agent = EmbeddedAgent::new(
            r#"{"provider":"mock","model":"m","maxTurns":1,"mockScript":[
                {"kind":"tool_use","toolUseId":"t1","name":"e","args":"{}"},
                {"kind":"tool_use","toolUseId":"t2","name":"e","args":"{}"},
                {"kind":"tool_use","toolUseId":"t3","name":"e","args":"{}"}
            ]}"#
            .into(),
        )
        .unwrap();
        // Closure tool (not pump): the guard test needs turns to advance
        // without a host resolving pump calls.
        let cb = js_sys::Function::new_no_args("return Promise.resolve('{\"text\":\"ok\"}')");
        agent
            .register_tool(
                "e".into(),
                "d".into(),
                r#"{"type":"object"}"#.into(),
                Some(cb),
            )
            .unwrap();
        agent.prompt("go".into());

        let mut saw_limit = false;
        let mut saw_end = false;
        for _ in 0..400 {
            TimeoutFuture::new(25).await;
            for line in agent.poll() {
                if line.contains(r#""error_type":"resource_limit""#) {
                    saw_limit = true;
                }
                if line.contains(r#""type":"agent_end""#) {
                    saw_end = true;
                }
            }
            if saw_limit && saw_end {
                break;
            }
        }
        assert!(saw_limit, "resource_limit error must be surfaced");
        assert!(
            saw_end,
            "run must terminate (agent_end) after the guard fires"
        );
    }
}
