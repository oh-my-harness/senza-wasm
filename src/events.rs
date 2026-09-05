//! Event serialization — `AgentEvent` → JSON lines (wire format).
//!
//! The `type` strings and field names mirror Senza's Python SDK
//! (`Senza/src/shared/event_stream.rs::agent_event_to_dict`) 1:1 so the
//! TS wrapper layer stays symmetric with the Python side.
//!
//! `AgentEvent` is `#[non_exhaustive]`: unknown variants serialize to
//! `{"type":"unknown","debug":...}` — never error the poll loop.

use llm_harness_types::{
    AgentError, AgentEvent, AgentMessage, ContentBlock, DataBlock, ToolProgress, ToolResult,
};

/// Serialize one `AgentEvent` as a single-line JSON string.
pub fn event_to_json(event: &AgentEvent) -> String {
    serde_json::to_string(&event_to_value(event)).unwrap_or_else(|_| {
        serde_json::json!({ "type": "unknown", "debug": "serialization failed" }).to_string()
    })
}

/// Serialize one `AgentEvent` as a JSON value (internal; `event_to_json`
/// wraps this). Exposed for tests.
fn event_to_value(event: &AgentEvent) -> serde_json::Value {
    match event {
        AgentEvent::AgentStart => serde_json::json!({ "type": "agent_start" }),
        AgentEvent::AgentEnd { new_messages } => serde_json::json!({
            "type": "agent_end",
            "new_messages_count": new_messages.len(),
            "new_messages": new_messages.iter().map(message_to_value).collect::<Vec<_>>(),
        }),
        AgentEvent::TurnStart { index } => {
            serde_json::json!({ "type": "turn_start", "index": index })
        }
        AgentEvent::TurnEnd {
            index,
            message,
            tool_results,
        } => {
            let results: serde_json::Map<String, serde_json::Value> = tool_results
                .iter()
                .map(|(id, res)| {
                    let v = match res {
                        Ok(tr) => {
                            let mut v = tool_result_to_value(tr);
                            v["ok"] = serde_json::Value::Bool(true);
                            v
                        }
                        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
                    };
                    (id.clone(), v)
                })
                .collect();
            serde_json::json!({
                "type": "turn_end",
                "index": index,
                "message_text": message.text_content(),
                "tool_results": results,
            })
        }
        AgentEvent::MessageStart { message_id } => {
            serde_json::json!({ "type": "message_start", "message_id": message_id })
        }
        AgentEvent::MessageUpdate {
            message_id,
            partial,
        } => serde_json::json!({
            "type": "message_update",
            "message_id": message_id,
            "text": partial.text_content(),
        }),
        AgentEvent::MessageEnd {
            message_id,
            message,
            ..
        } => serde_json::json!({
            "type": "message_end",
            "message_id": message_id,
            "text": message.text_content(),
        }),
        AgentEvent::TextDelta { message_id, text } => serde_json::json!({
            "type": "text_delta", "message_id": message_id, "text": text,
        }),
        AgentEvent::ThinkingDelta {
            message_id,
            thinking,
            signature,
        } => {
            let mut v = serde_json::json!({
                "type": "thinking_delta",
                "message_id": message_id,
                "thinking": thinking,
            });
            if let Some(sig) = signature {
                v["signature"] = serde_json::Value::String(sig.clone());
            }
            v
        }
        AgentEvent::ToolCallStart {
            message_id,
            tool_use_id,
            name,
        } => serde_json::json!({
            "type": "tool_call_start",
            "message_id": message_id,
            "tool_use_id": tool_use_id,
            "tool_name": name,
        }),
        AgentEvent::ToolCallArgsDelta {
            tool_use_id,
            partial_input,
        } => serde_json::json!({
            "type": "tool_call_args_delta",
            "tool_use_id": tool_use_id,
            "partial_input": partial_input,
        }),
        AgentEvent::ToolCallEnd { tool_use_id, args } => serde_json::json!({
            "type": "tool_call_end", "tool_use_id": tool_use_id, "args": args,
        }),
        AgentEvent::ToolExecutionStart {
            tool_use_id,
            tool_name,
            args,
        } => serde_json::json!({
            "type": "tool_execution_start",
            "tool_use_id": tool_use_id,
            "tool_name": tool_name,
            "args": args,
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_use_id,
            partial,
        } => serde_json::json!({
            "type": "tool_execution_update",
            "tool_use_id": tool_use_id,
            "result": progress_to_value(partial),
        }),
        AgentEvent::ToolExecutionEnd {
            tool_use_id,
            result,
        } => {
            let mut v = serde_json::json!({
                "type": "tool_execution_end", "tool_use_id": tool_use_id,
            });
            match result {
                Ok(tr) => {
                    v["ok"] = serde_json::Value::Bool(true);
                    v["result"] = tool_result_to_value(tr);
                }
                Err(e) => {
                    v["ok"] = serde_json::Value::Bool(false);
                    v["error"] = serde_json::Value::String(e.to_string());
                }
            }
            v
        }
        AgentEvent::Error(err) => {
            let mut v = serde_json::json!({
                "type": "error", "message": err.to_string(),
            });
            if let Some(kind) = error_type(err) {
                v["error_type"] = serde_json::Value::String(kind.to_string());
            }
            v
        }
        AgentEvent::RetryAttempt {
            attempt,
            max_retries,
            delay_ms,
            error,
        } => serde_json::json!({
            "type": "retry_attempt",
            "attempt": attempt,
            "max_retries": max_retries,
            "delay_ms": delay_ms,
            "error": error,
        }),
        _ => serde_json::json!({ "type": "unknown", "debug": format!("{event:?}") }),
    }
}

/// Error classification, mirroring Senza's `error_type` mapping.
fn error_type(err: &AgentError) -> Option<&'static str> {
    #[allow(unreachable_patterns)] // forward-compat catch-all for non_exhaustive
    Some(match err {
        AgentError::ProviderTyped { .. } => "provider",
        AgentError::Tool { .. } => "tool",
        AgentError::Aborted => "aborted",
        AgentError::NotIdle => "not_idle",
        AgentError::InvalidInput(_) => "invalid_input",
        AgentError::Internal(_) => "internal",
        AgentError::ResourceLimitExceeded(_) => "resource_limit",
        AgentError::StreamIdle { .. } => "stream_idle",
        AgentError::FinalAnswerRejected { .. } => "final_answer_rejected",
        // non_exhaustive: future kernel variants fall through to None.
        // clippy can see they are currently unreachable; the arm stays for
        // forward compatibility across kernel rev bumps.
        _ => return None,
    })
}

/// `AgentMessage` → `{role, text}` (mirror of `agent_message_to_dict`).
/// Also the export_session projection (facade-owned session JSON).
pub(crate) fn message_to_value(msg: &AgentMessage) -> serde_json::Value {
    #[allow(unreachable_patterns)] // forward-compat catch-all for non_exhaustive
    let (role, text) = match msg {
        AgentMessage::User(m) => ("user", join_blocks_text(&m.content)),
        AgentMessage::Assistant(m) => ("assistant", m.text_content()),
        AgentMessage::ToolResult(m) => ("tool_result", join_data_blocks_text(&m.content)),
        AgentMessage::BranchSummary(m) => ("branch_summary", m.summary.clone()),
        AgentMessage::CompactionSummary(m) => ("compaction_summary", m.summary.clone()),
        AgentMessage::Custom(m) => ("custom", serde_json::to_string(&m.data).unwrap_or_default()),
        // non_exhaustive: future kernel variants project to unknown.
        _ => ("unknown", String::new()),
    };
    serde_json::json!({ "role": role, "text": text })
}

fn join_blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn join_data_blocks_text(blocks: &[DataBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            DataBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `ToolResult` → flat `{terminate, text, details}` (mirror of
/// `tool_result_to_flat_dict`).
fn tool_result_to_value(result: &ToolResult) -> serde_json::Value {
    let text = result
        .model_content
        .iter()
        .filter_map(|b| match b {
            DataBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::json!({
        "terminate": result.terminate,
        "text": text,
        "details": result.details,
    })
}

/// `ToolProgress` → `{terminate, text, details}` (mirror of the
/// `tool_execution_update` projection).
fn progress_to_value(progress: &ToolProgress) -> serde_json::Value {
    let text = progress
        .content
        .iter()
        .filter_map(|b| match b {
            DataBlock::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::json!({
        "terminate": false,
        "text": text,
        "details": progress.details,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use llm_harness_types::UserMessage;

    fn parse(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn text_delta_serializes_with_type_tag() {
        let ev = AgentEvent::TextDelta {
            message_id: "m1".into(),
            text: "hi".into(),
        };
        let v = parse(&event_to_json(&ev));
        assert_eq!(v["type"], "text_delta");
        assert_eq!(v["message_id"], "m1");
        assert_eq!(v["text"], "hi");
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn error_serializes_message_and_type() {
        let ev = AgentEvent::Error(AgentError::Aborted);
        let v = parse(&event_to_json(&ev));
        assert_eq!(v["type"], "error");
        assert_eq!(v["error_type"], "aborted");
        assert!(v["message"].is_string());
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn tool_execution_end_ok_and_err_arms() {
        let ok = AgentEvent::ToolExecutionEnd {
            tool_use_id: "t1".into(),
            result: Ok(ToolResult::full(
                vec![DataBlock::text("done")],
                serde_json::json!({"k": 1}),
                false,
            )),
        };
        let v = parse(&event_to_json(&ok));
        assert_eq!(v["type"], "tool_execution_end");
        assert_eq!(v["ok"], true);
        assert_eq!(v["result"]["text"], "done");
        assert_eq!(v["result"]["details"]["k"], 1);

        let err = AgentEvent::ToolExecutionEnd {
            tool_use_id: "t2".into(),
            result: Err(llm_harness_types::ToolFailure::new("boom", "bad")),
        };
        let v = parse(&event_to_json(&err));
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"], "boom: bad"); // ToolFailure Display = "{code}: {model_message}"
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn agent_end_carries_message_list() {
        let ev = AgentEvent::AgentEnd {
            new_messages: vec![AgentMessage::User(UserMessage {
                content: vec![ContentBlock::Text { text: "q".into() }],
                timestamp: Utc::now(),
            })],
        };
        let v = parse(&event_to_json(&ev));
        assert_eq!(v["type"], "agent_end");
        assert_eq!(v["new_messages_count"], 1);
        assert_eq!(v["new_messages"][0]["role"], "user");
        assert_eq!(v["new_messages"][0]["text"], "q");
    }

    #[wasm_bindgen_test::wasm_bindgen_test]
    fn json_output_is_single_line() {
        let ev = AgentEvent::TextDelta {
            message_id: "m".into(),
            text: "line1\nline2".into(),
        };
        let json = event_to_json(&ev);
        assert_eq!(json.lines().count(), 1);
        assert!(json.contains("\\n"));
    }
}
