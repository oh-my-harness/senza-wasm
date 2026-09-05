import assert from "node:assert";
import { EmbeddedAgent } from "../pkg-node/senza_wasm.js";
import { Deck, makeDeckTools } from "../demo/deck.js";

// —— 1. Deck 模型：乱序写、覆盖、删除、大纲、错误文案 ——
{
  const d = new Deck();
  d.write(2, "B", "<section>b</section>");
  d.write(1, "A", "<section>a</section>");
  assert.strictEqual(d.outline(), "1. A\n2. B", "乱序写入按页码排序");
  d.write(1, "A2", "<section>a2</section>");
  assert.strictEqual(d.outline(), "1. A2\n2. B", "同 index 覆盖");
  assert.strictEqual(d.remove(2), null, "删除存在页返回 null");
  assert.strictEqual(d.outline(), "1. A2", "删除后大纲更新");
  assert.strictEqual(d.maxIndex(), 1, "maxIndex");
  assert.match(d.remove(9), /第 9 页不存在/, "删除缺失页返回错误文案");
  assert.strictEqual(new Deck().outline(), "（空）", "空大纲");
}

// —— 2. 工具闭包全链路（mock provider）：写×2 → 删 → 收尾 ——
{
  const deck = new Deck();
  const written = [];
  const agent = new EmbeddedAgent(JSON.stringify({
    provider: "mock", model: "mock-model",
    mockScript: [
      { kind: "tool_use", toolUseId: "t1", name: "writeSlide",
        args: JSON.stringify({ index: 1, title: "封面", html: "<section><h1>Hi</h1></section>" }) },
      { kind: "tool_use", toolUseId: "t2", name: "writeSlide",
        args: JSON.stringify({ index: 2, title: "要点", html: "<section><ul><li>x</li></ul></section>" }) },
      { kind: "tool_use", toolUseId: "t3", name: "deleteSlide", args: JSON.stringify({ index: 1 }) },
      { kind: "text", text: "done" },
    ],
  }));
  makeDeckTools(deck, { onWrite: (i) => written.push(i) }).register(agent);
  agent.prompt("做一份两页 deck");

  const results = [];
  for (let i = 0; i < 400; i++) {
    await new Promise((r) => setTimeout(r, 25));
    let end = false;
    for (const l of agent.poll()) {
      const ev = JSON.parse(l);
      if (ev.type === "tool_execution_end" && ev.ok) results.push(ev.result?.text ?? "");
      if (ev.type === "agent_end") end = true;
    }
    if (end) break;
  }
  assert(results.some((t) => t.includes("已写入第 1 页「封面」")), "写结果确认动作: " + results.join("|"));
  assert(results.some((t) => t.includes("2. 要点")), "工具结果回显大纲: " + results.join("|"));
  assert(results.every((t) => !t.includes("ERROR")), "全链路无失败操作: " + results.join("|"));
  assert.deepStrictEqual(written, [1, 2], "onWrite 钩子按序触发");
  assert.deepStrictEqual(deck.sorted().map(([i]) => i), [2], "deleteSlide 真删了第 1 页");
}

console.log("DECK MODEL & TOOLS PASS");
