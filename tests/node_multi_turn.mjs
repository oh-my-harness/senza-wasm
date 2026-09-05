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
}

await main();
