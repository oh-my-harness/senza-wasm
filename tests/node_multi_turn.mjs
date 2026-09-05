import assert from "node:assert";
import { EmbeddedAgent } from "../pkg-node/senza_wasm.js";

async function runTo(agent, needle) {
  const lines = [];
  for (let i = 0; i < 200; i++) {
    await new Promise((r) => setTimeout(r, 25));
    lines.push(...agent.poll());
    if (lines.some((l) => l.includes(needle))) return lines;
  }
  throw new Error("timeout waiting for " + needle + ":\n" + lines.join("\n"));
}

async function main() {
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

  agent.prompt("hi");
  let lines = await runTo(agent, '"type":"agent_end"');
  assert(lines.some((l) => l.includes('"text":"first"')), "first script item:\n" + lines.join("\n"));

  agent.prompt("again");
  lines = await runTo(agent, '"type":"agent_end"');
  assert(lines.some((l) => l.includes('"text":"second"')), "second script item:\n" + lines.join("\n"));
  console.log("MOCK SCRIPT PASS");

  // --- history continuation: two prompts, each consumes exactly one
  // script entry (an amnesiac or double-consuming facade breaks this).
  const agent2 = new EmbeddedAgent(JSON.stringify({
    provider: "mock",
    model: "mock-model",
    mockScript: [
      { kind: "text", text: "answer-one" },
      { kind: "text", text: "answer-two" },
    ],
  }));
  agent2.prompt("question-one");
  const first = await runTo(agent2, '"type":"agent_end"');
  assert(!first.some((l) => l.includes('"text":"answer-two"')),
    "script must not skip ahead on first turn:\n" + first.join("\n"));
  agent2.prompt("question-two");
  const second = await runTo(agent2, '"type":"agent_end"');
  assert(second.some((l) => l.includes('"text":"answer-two"')),
    "second turn must use second script entry:\n" + second.join("\n"));
  assert(second.some((l) => l.includes('"type":"text_delta"')),
    "second turn produced text");
  console.log("HISTORY CONTINUATION PASS");

  // --- multi-turn tool conversation, pump path (callback = null):
  // tool_execution_start → resolveTool → second turn → agent_end.
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
  const seq = [];
  for (let i = 0; i < 400 && !seq.includes("agent_end"); i++) {
    await new Promise((r) => setTimeout(r, 25));
    for (const l of agent3.poll()) {
      const ev = JSON.parse(l);
      seq.push(ev.type);
      if (ev.type === "tool_execution_start") {
        assert.strictEqual(ev.tool_use_id, "t1", "pump addressing field is tool_use_id");
        agent3.resolveTool(ev.tool_use_id, JSON.stringify({ text: "echoed:hi" }));
      }
    }
  }
  assert(seq.includes("tool_execution_start"), "pump path surfaced tool call:\n" + seq.join(" -> "));
  assert(seq.includes("text_delta"), "second turn produced text:\n" + seq.join(" -> "));
  assert(seq.includes("agent_end"), "run completed:\n" + seq.join(" -> "));
  const t1 = seq.indexOf("tool_execution_start");
  const t2 = seq.indexOf("turn_start", t1);
  assert(t2 > t1, "resolveTool advanced to a second turn:\n" + seq.join(" -> "));
  console.log("MULTI-TURN TOOL (pump) PASS");

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
  for (let i = 0; i < 400 && !seq4.includes("agent_end"); i++) {
    await new Promise((r) => setTimeout(r, 25));
    for (const l of agent4.poll()) seq4.push(JSON.parse(l).type);
  }
  assert(seq4.includes("tool_execution_start") && seq4.includes("text_delta") && seq4.includes("agent_end"),
    "closure tool auto-completed two turns:\n" + seq4.join(" -> "));
  console.log("MULTI-TURN TOOL (closure) PASS");

  // --- error path: resolveTool with unknown id → JsValue error ---
  const agent5 = new EmbeddedAgent(JSON.stringify({ provider: "mock", model: "mock-model" }));
  assert.throws(() => agent5.resolveTool("nonexistent-id", "{}"),
    /unknown or already-resolved tool_use_id/);

  // --- error path: endless tool_use script + maxTurns=2 → resource_limit ---
  const agent6 = new EmbeddedAgent(JSON.stringify({
    provider: "mock",
    model: "mock-model",
    maxTurns: 2,
    // Script must outlast the guard: if it runs dry, MockLlmClient's
    // fallback text response ends the run with EndTurn before the
    // next-turn abort check can fire (the kernel loop checks abort at
    // the TOP of each turn, and the loop itself has no max_turns —
    // loop_safety lives in the strategy layer, facade enforces here).
    mockScript: [
      { kind: "tool_use", toolUseId: "t1", name: "echo", args: "{}" },
      { kind: "tool_use", toolUseId: "t2", name: "echo", args: "{}" },
      { kind: "tool_use", toolUseId: "t3", name: "echo", args: "{}" },
      { kind: "tool_use", toolUseId: "t4", name: "echo", args: "{}" },
      { kind: "tool_use", toolUseId: "t5", name: "echo", args: "{}" },
      { kind: "tool_use", toolUseId: "t6", name: "echo", args: "{}" },
    ],
  }));
  agent6.registerTool("echo", "returns its input", JSON.stringify({ type: "object" }),
    async () => JSON.stringify({ text: "ok" }));
  agent6.prompt("go");
  let sawLimit = false;
  for (let i = 0; i < 400 && !sawLimit; i++) {
    await new Promise((r) => setTimeout(r, 25));
    for (const l of agent6.poll()) {
      const ev = JSON.parse(l);
      if (ev.type === "error" && ev.error_type === "resource_limit") sawLimit = true;
    }
  }
  assert(sawLimit, "max_turns guard fired (no resource_limit error seen)");
  console.log("ERROR PATHS PASS");
}

await main();
