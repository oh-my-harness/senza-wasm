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
    AgentContext, AgentEvent, RunContext, RunRequest, StreamOptions, ToolExecutionMode,
};
use tokio_util::sync::CancellationToken;

use crate::events::event_to_json;
use crate::tools::{JsTool, PendingTool};

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
    /// Pump tools: call_id → result channel. `resolve_tool` pushes here;
    /// the PendingTool's receiver (inside the Tool) awaits it.
    pub pump_tx: std::collections::HashMap<String, futures::channel::mpsc::Sender<String>>,
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
            pump_tx: std::collections::HashMap::new(),
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
#[serde(tag = "kind", rename_all = "snake_case")]
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
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    base_url: Option<String>,
    model: String,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default = "default_max_tokens")]
    max_tokens: u32,
    #[serde(default = "default_max_turns")]
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
                // Pump placeholder. The loop keys tool calls by tool_use_id
                // (the LLM-assigned id), so the placeholder is created
                // per-registration with a fresh channel; resolve_tool
                // addresses it by tool_use_id.
                let call_id = uuid::Uuid::new_v4().to_string();
                let (tx, rx) = futures::channel::mpsc::channel::<String>(1);
                state.pump_tx.insert(call_id.clone(), tx);
                state
                    .tools
                    .push(PendingTool::new(name, description, schema, call_id, rx));
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
    pub fn is_running(&self) -> bool {
        self.state.lock().running
    }

    /// Resolve a pump tool call: push `result_json` into the channel the
    /// pending tool's `execute` is awaiting. `tool_use_id` is the id from
    /// the `tool_execution_start` event.
    pub fn resolve_tool(&self, tool_use_id: String, result_json: String) -> Result<(), JsValue> {
        let mut state = self.state.lock();
        match state.pump_tx.remove(&tool_use_id) {
            Some(mut tx) => tx
                .try_send(result_json)
                .map_err(|e| JsValue::from_str(&format!("resolve_tool failed: {e}"))),
            None => Err(JsValue::from_str(&format!(
                "resolve_tool: unknown or already-resolved tool_use_id '{tool_use_id}'"
            ))),
        }
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
