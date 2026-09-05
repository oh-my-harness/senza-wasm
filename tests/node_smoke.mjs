import assert from "node:assert";
import { EmbeddedAgent } from "../pkg-node/senza_wasm.js";

const agent = new EmbeddedAgent(JSON.stringify({ provider: "mock", model: "mock-model" }));
agent.prompt("hi");
const lines = [];
for (let i = 0; i < 100; i++) {
  await new Promise((r) => setTimeout(r, 50));
  lines.push(...agent.poll());
  if (lines.some((l) => l.includes('"type":"agent_end"'))) break;
}
assert(lines.some((l) => l.includes('"type":"text_delta"')), "no text_delta:\n" + lines.join("\n"));
assert(lines.some((l) => l.includes('"type":"agent_end"')), "no agent_end:\n" + lines.join("\n"));
const types = lines.map((l) => JSON.parse(l).type);
console.log("NODE SMOKE PASS —", types.join(" -> "));
