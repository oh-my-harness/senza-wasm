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
 }

await main();
