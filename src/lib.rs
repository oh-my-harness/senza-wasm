//! senza-wasm: TypeScript binding for the llm-harness agent loop.
//!
//! All `#[wasm_bindgen]` exports live in this crate; the kernel crates
//! (llm-harness-loop / llm-harness-types) stay binding-free
//! (senza-wasm spec §2 decision 4).
//!
//! Event wire format is JSON lines — see `events::event_to_json`. Tool
//! callbacks arrive as JS closures (`tools::JsTool`) or as pump calls
//! (`EmbeddedAgent::resolve_tool`) — see spec §4.2.

pub mod agent;
pub mod events;
pub mod tools;

use wasm_bindgen::prelude::*;

/// Better panic messages in the browser/Node console (debug + release).
#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// Crate version, surfaced to JS for diagnostics.
#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
