//! Tool bridge — JS callbacks as `Arc<dyn Tool>` + the pump registry.
//!
//! Two host integration paths (spec §4.2):
//! - **Closure path**: `JsTool` wraps a JS async callback
//!   `(args_json) => Promise<result_json>`; the loop calls it directly.
//! - **Pump path**: `PendingTool` registers a placeholder; the loop
//!   surfaces `tool_call` events and the host completes them via
//!   `EmbeddedAgent::resolve_tool` — no JS closure crosses the boundary.

use futures::StreamExt;
use llm_harness_types::{DataBlock, Tool, ToolContext, ToolFailure, ToolFuture, ToolResult};
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

/// A tool backed by a JS async callback:
/// `(args_json: string) => Promise<string>` where the resolved string is
/// the tool result text.
pub struct JsTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))] // executed on wasm only
    callback: js_sys::Function,
}

impl JsTool {
    pub fn new(
        name: String,
        description: String,
        schema: serde_json::Value,
        callback: js_sys::Function,
    ) -> Self {
        Self {
            name,
            description,
            schema,
            callback,
        }
    }
}

impl Tool for JsTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> &serde_json::Value {
        &self.schema
    }

    #[cfg(target_arch = "wasm32")]
    fn execute<'a>(&'a self, args: serde_json::Value, _ctx: &'a ToolContext) -> ToolFuture<'a> {
        let callback = self.callback.clone();
        Box::pin(async move {
            let args_js = JsValue::from_str(&args.to_string());
            let ret = callback.call1(&JsValue::NULL, &args_js).map_err(|e| {
                ToolFailure::new("execution_error", format!("JS callback failed: {e:?}"))
            })?;
            // Host callbacks may be sync (plain string/object return) or async
            // (Promise). Promise::from on a non-thenable corrupts JsFuture's
            // .then() call — the run task panics and `running` never resets.
            // Normalize: wrap non-promises in Promise.resolve().
            let promise = if ret.is_instance_of::<js_sys::Promise>() {
                js_sys::Promise::from(ret)
            } else {
                js_sys::Promise::resolve(&ret)
            };
            let out = JsFuture::from(promise).await.map_err(|e| {
                ToolFailure::new("execution_error", format!("JS promise rejected: {e:?}"))
            })?;
            let out_str = out.as_string().unwrap_or_default();
            let value: serde_json::Value = serde_json::from_str(&out_str).map_err(|e| {
                ToolFailure::new(
                    "invalid_result",
                    format!("ToolResult JSON parse failed: {e}"),
                )
            })?;
            let text = value
                .get("text")
                .and_then(|t| t.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| out_str.clone());
            let details = value.get("details").cloned().unwrap_or_else(|| {
                serde_json::from_str(&out_str).unwrap_or(serde_json::Value::Null)
            });
            let terminate = value
                .get("terminate")
                .and_then(|t| t.as_bool())
                .unwrap_or(false);
            Ok(ToolResult::full(
                vec![DataBlock::text(text)],
                details,
                terminate,
            ))
        })
    }

    /// JS callbacks only exist inside a JS runtime; the native `Tool`
    /// signature requires `Send`, which `JsValue` can never be. Compiling
    /// the facade for a native target is meaningless anyway — fail loudly
    /// instead of pretending.
    #[cfg(not(target_arch = "wasm32"))]
    fn execute<'a>(&'a self, _args: serde_json::Value, _ctx: &'a ToolContext) -> ToolFuture<'a> {
        unreachable!("JsTool requires a JS runtime — build for wasm32")
    }
}

/// Pump-path placeholder: `execute` creates a fresh one-shot channel and
/// registers its receiver under the LLM-assigned `tool_use_id` that the
/// loop passes in `ToolContext`; the host answers with
/// `resolve_tool(tool_use_id, result_json)`, which the facade routes to
/// the awaiting receiver. No JS closure crosses the boundary.
pub struct PendingTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    /// `tool_use_id` → sender for the in-flight call. `execute` inserts;
    /// `resolve` (via the facade) removes and fires.
    pending: parking_lot::Mutex<
        std::collections::HashMap<String, futures::channel::mpsc::Sender<String>>,
    >,
}

impl PendingTool {
    pub fn new(name: String, description: String, schema: serde_json::Value) -> Arc<Self> {
        Arc::new(Self {
            name,
            description,
            schema,
            pending: parking_lot::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Route `result_json` to the execute awaiting `tool_use_id`.
    pub fn resolve(&self, tool_use_id: &str, result_json: String) -> Result<(), String> {
        match self.pending.lock().remove(tool_use_id) {
            Some(mut tx) => tx
                .try_send(result_json)
                .map_err(|e| format!("resolve failed: {e}")),
            None => Err(format!(
                "unknown or already-resolved tool_use_id '{tool_use_id}'"
            )),
        }
    }
}

impl Tool for PendingTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> &serde_json::Value {
        &self.schema
    }

    fn execute<'a>(&'a self, _args: serde_json::Value, ctx: &'a ToolContext) -> ToolFuture<'a> {
        // Fresh one-shot channel per call, keyed by the LLM-assigned id;
        // `PendingTool::resolve` fires it when the host answers.
        let (tx, mut rx) = futures::channel::mpsc::channel::<String>(1);
        self.pending.lock().insert(ctx.tool_use_id.clone(), tx);
        let call_id = ctx.tool_use_id.clone();
        Box::pin(async move {
            match rx.next().await {
                Some(result_json) => parse_pump_result(&call_id, &result_json),
                None => Err(ToolFailure::new(
                    "pump_cancelled",
                    format!("tool call {call_id} was cancelled"),
                )),
            }
        })
    }
}
// ToolFailure is 144 bytes on native targets (kernel type, its size is
// the Tool::execute contract — not ours to shrink).
#[allow(clippy::result_large_err)]
fn parse_pump_result(call_id: &str, result_json: &str) -> Result<ToolResult, ToolFailure> {
    let value: serde_json::Value = serde_json::from_str(result_json).map_err(|e| {
        ToolFailure::new(
            "invalid_result",
            format!("resolveTool JSON parse failed: {e}"),
        )
    })?;
    let text = value
        .get("text")
        .and_then(|t| t.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| result_json.to_string());
    let details = value
        .get("details")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let terminate = value
        .get("terminate")
        .and_then(|t| t.as_bool())
        .unwrap_or(false);
    let _ = call_id;
    Ok(ToolResult::full(
        vec![DataBlock::text(text)],
        details,
        terminate,
    ))
}

/// Host approval gate (M3 spec §3.3): wraps a tool in `HookedTool` with a
/// before-hook that parks a one-shot channel under the LLM-assigned
/// `tool_use_id` until the host calls `approveToolCall(id, allow)`.
/// allow → `Allow` (inner tool runs); deny → `Deny(denied_by_host)`,
/// which the kernel turns into a `ToolFailure` visible to the LLM.
pub struct ApprovalGate {
    /// `tool_use_id` → sender for the pending approval decision.
    pending: parking_lot::Mutex<
        std::collections::HashMap<String, futures::channel::oneshot::Sender<bool>>,
    >,
}

impl ApprovalGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: parking_lot::Mutex::new(std::collections::HashMap::new()),
        })
    }

    /// Fire the host's decision for `tool_use_id`. Errors if the id is
    /// unknown (no parked call) or already answered.
    pub fn approve(&self, tool_use_id: &str, allow: bool) -> Result<(), String> {
        match self.pending.lock().remove(tool_use_id) {
            Some(tx) => tx
                .send(allow)
                .map_err(|_| format!("approval receiver dropped for '{tool_use_id}'")),
            None => Err(format!(
                "unknown or already-approved tool_use_id '{tool_use_id}'"
            )),
        }
    }
}

impl llm_harness_types::BeforeToolCallHook for ApprovalGate {
    fn on_call<'a>(
        &'a self,
        ctx: llm_harness_types::BeforeToolCallCtx<'a>,
    ) -> futures::future::BoxFuture<'a, llm_harness_types::BeforeToolCallDecision> {
        let (tx, rx) = futures::channel::oneshot::channel::<bool>();
        self.pending.lock().insert(ctx.tool_use_id.to_string(), tx);
        Box::pin(async move {
            match rx.await {
                Ok(true) => llm_harness_types::BeforeToolCallDecision::Allow,
                Ok(false) => llm_harness_types::BeforeToolCallDecision::Deny(ToolFailure::new(
                    "denied_by_host",
                    "The host denied this tool call.",
                )),
                // Receiver dropped without a decision (agent dropped):
                // deny rather than hang.
                Err(_) => llm_harness_types::BeforeToolCallDecision::Deny(ToolFailure::new(
                    "denied_by_host",
                    "Approval gate dropped before a decision arrived.",
                )),
            }
        })
    }
}

/// Wrap `inner` with the host approval gate (M3). The returned tool keeps
/// the inner tool's identity (name/description/schema) — `HookedTool`
/// delegates the `Tool` surface and only intercepts execution.
pub fn with_approval(inner: Arc<dyn Tool>, gate: Arc<ApprovalGate>) -> Arc<dyn Tool> {
    Arc::new(llm_harness_loop::HookedTool {
        inner,
        before: Some(gate),
        after: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use llm_harness_types::{RunContext, RunRequest, ToolContext};
    use std::sync::Arc;

    fn make_ctx() -> ToolContext {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        ToolContext {
            run: Arc::new(RunContext::new(RunRequest::default())),
            env: Arc::new(llm_harness_loop::test_utils::NoOpEnv),
            abort: tokio_util::sync::CancellationToken::new(),
            tool_use_id: "c1".into(),
            turn_index: 0,
            assistant_message: Arc::new(llm_harness_loop::test_utils::test_assistant_message(
                vec![],
            )),
            update_tx: tx,
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn js_tool_trait_surface() {
        let cb = js_sys::Function::new_no_args("return Promise.resolve('{\"text\":\"ok\"}');");
        let tool = JsTool::new(
            "t".into(),
            "d".into(),
            serde_json::json!({"type":"object"}),
            cb,
        );
        assert_eq!(Tool::name(&tool), "t");
        assert_eq!(Tool::description(&tool), "d");
        assert_eq!(
            tool.parameters_schema(),
            &serde_json::json!({"type":"object"})
        );
    }

    /// Regression: a SYNC host callback (plain string return, no Promise)
    /// used to corrupt JsFuture (`arg0.then is not a function`), killing the
    /// run task with `running` never reset — the agent locked up forever.
    /// Promise::resolve must normalize non-thenable returns.
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn js_tool_sync_callback_completes() {
        let cb = js_sys::Function::new_no_args("return '{\"text\":\"sync ok\"}';");
        let tool = JsTool::new(
            "t".into(),
            "d".into(),
            serde_json::json!({"type":"object"}),
            cb,
        );
        let ctx = make_ctx();
        let fut = Tool::execute(&tool, serde_json::json!({}), &ctx);
        let result = fut.await.unwrap();
        assert_eq!(
            result.model_content.first().and_then(|b| match b {
                DataBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            }),
            Some("sync ok")
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn pending_tool_resolves_after_host_reply() {
        let tool = PendingTool::new("echo".into(), "pump echo".into(), serde_json::json!({}));
        assert_eq!(Tool::name(tool.as_ref()), "echo");
        // execute parks a one-shot sender under the ctx tool_use_id;
        // resolve() must fire exactly that channel.
        let ctx = make_ctx();
        let fut = Tool::execute(tool.as_ref(), serde_json::json!({}), &ctx);
        tool.resolve("c1", "{\"text\":\"pong\"}".into()).unwrap();
        let result = futures::executor::block_on(fut).unwrap();
        assert_eq!(result.model_content.len(), 1);
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn pending_tool_resolve_unknown_id_errors() {
        let tool = PendingTool::new("echo".into(), "pump echo".into(), serde_json::json!({}));
        assert!(tool.resolve("nope", "{}".into()).is_err());
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn approval_gate_allow_and_deny() {
        use futures::future::BoxFuture;
        use llm_harness_types::{
            BeforeToolCallCtx, BeforeToolCallDecision, BeforeToolCallHook, RunContext, RunRequest,
        };

        let ctx = make_ctx();
        let run = Arc::new(RunContext::new(RunRequest::default()));
        let before_ctx = BeforeToolCallCtx {
            run: &run,
            assistant_message: &ctx.assistant_message,
            tool_use_id: "c1",
            tool_name: "danger",
            args: &serde_json::json!({}),
            turn_index: 0,
        };

        // allow path
        let gate = ApprovalGate::new();
        let fut: BoxFuture<'_, BeforeToolCallDecision> =
            BeforeToolCallHook::on_call(&*gate, before_ctx);
        gate.approve("c1", true).unwrap();
        assert!(matches!(
            futures::executor::block_on(fut),
            BeforeToolCallDecision::Allow
        ));

        // deny path (fresh ctx — on_call takes it by value)
        let gate = ApprovalGate::new();
        let before_ctx = BeforeToolCallCtx {
            run: &run,
            assistant_message: &ctx.assistant_message,
            tool_use_id: "c1",
            tool_name: "danger",
            args: &serde_json::json!({}),
            turn_index: 0,
        };
        let fut: BoxFuture<'_, BeforeToolCallDecision> =
            BeforeToolCallHook::on_call(&*gate, before_ctx);
        gate.approve("c1", false).unwrap();
        match futures::executor::block_on(fut) {
            BeforeToolCallDecision::Deny(f) => {
                assert_eq!(f.code, "denied_by_host");
            }
            _ => panic!("expected Deny"),
        }

        // unknown id
        assert!(gate.approve("nope", true).is_err());
    }
}
