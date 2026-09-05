//! Tool bridge — JS callbacks as `Arc<dyn Tool>` + the pump registry.
//!
//! Two host integration paths (spec §4.2):
//! - **Closure path**: `JsTool` wraps a JS async callback
//!   `(args_json) => Promise<result_json>`; the loop calls it directly.
//! - **Pump path**: `PendingTool` registers a placeholder; the loop
//!   surfaces `tool_call` events and the host completes them via
//!   `EmbeddedAgent::resolve_tool` — no JS closure crosses the boundary.

use std::sync::Arc;
use llm_harness_types::{DataBlock, Tool, ToolContext, ToolFailure, ToolFuture, ToolResult};
use wasm_bindgen::prelude::*;
use futures::StreamExt;
use wasm_bindgen_futures::JsFuture;

/// A tool backed by a JS async callback:
/// `(args_json: string) => Promise<string>` where the resolved string is
/// the tool result text.
pub struct JsTool {
    name: String,
    description: String,
    schema: serde_json::Value,
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

    fn execute<'a>(
        &'a self,
        args: serde_json::Value,
        _ctx: &'a ToolContext,
    ) -> ToolFuture<'a> {
        let callback = self.callback.clone();
        Box::pin(async move {
            let args_js = JsValue::from_str(&args.to_string());
            let ret = callback
                .call1(&JsValue::NULL, &args_js)
                .map_err(|e| ToolFailure::new("execution_error", format!("JS callback failed: {e:?}")))?;
            let promise = js_sys::Promise::from(ret);
            let out = JsFuture::from(promise)
                .await
                .map_err(|e| ToolFailure::new("execution_error", format!("JS promise rejected: {e:?}")))?;
            let out_str = out.as_string().unwrap_or_default();
            let value: serde_json::Value = serde_json::from_str(&out_str)
                .map_err(|e| ToolFailure::new("invalid_result", format!("ToolResult JSON parse failed: {e}")))?;
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
}

/// Pump-path placeholder: `execute` awaits the channel until the host
/// calls `resolve_tool(call_id, result_json)` (agent pushes the result
/// JSON into the channel). The receiver is handed over on first execute
/// (take semantics): one tool call consumes one resolution. Channel
/// close (agent-side drop) = cancellation.
pub struct PendingTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    call_id: String,
    rx: parking_lot::Mutex<Option<futures::channel::mpsc::Receiver<String>>>,
}

impl PendingTool {
    pub fn new(
        name: String,
        description: String,
        schema: serde_json::Value,
        call_id: String,
        rx: futures::channel::mpsc::Receiver<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            description,
            schema,
            call_id,
            rx: parking_lot::Mutex::new(Some(rx)),
        })
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

    fn execute<'a>(
        &'a self,
        _args: serde_json::Value,
        _ctx: &'a ToolContext,
    ) -> ToolFuture<'a> {
        let call_id = self.call_id.clone();
        // One tool call = one receiver handoff; a second execute on the
        // same PendingTool (should not happen — the loop re-registers per
        // call) degrades to a cancelled failure rather than deadlocking.
        let mut guard = self.rx.lock();
        let mut rx = guard.take().unwrap_or_else(|| {
            let (_tx, rx) = futures::channel::mpsc::channel::<String>(1);
            rx
        });
        drop(guard);
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
fn parse_pump_result(call_id: &str, result_json: &str) -> Result<ToolResult, ToolFailure> {
    let value: serde_json::Value = serde_json::from_str(result_json)
        .map_err(|e| ToolFailure::new("invalid_result", format!("resolveTool JSON parse failed: {e}")))?;
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
        let tool = JsTool::new("t".into(), "d".into(), serde_json::json!({"type":"object"}), cb);
        assert_eq!(Tool::name(&tool), "t");
        assert_eq!(Tool::description(&tool), "d");
        assert_eq!(
            tool.parameters_schema(),
            &serde_json::json!({"type":"object"})
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn pending_tool_resolves_after_host_reply() {
        let (mut tx, rx) = futures::channel::mpsc::channel::<String>(1);
        let tool = PendingTool::new(
            "echo".into(),
            "pump echo".into(),
            serde_json::json!({}),
            "call-1".into(),
            rx,
        );
        assert_eq!(Tool::name(tool.as_ref()), "echo");
        tx.try_send("{\"text\":\"pong\"}".into()).unwrap();
        let ctx = make_ctx();
        let fut = Tool::execute(tool.as_ref(), serde_json::json!({}), &ctx);
        let result = futures::executor::block_on(fut).unwrap();
        assert_eq!(result.model_content.len(), 1);
    }
}
