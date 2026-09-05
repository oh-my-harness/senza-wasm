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

// —— 3. 组装器：缺页骨架、初始页、翻页通道、标题 ——
{
  const { buildDeckDocument } = await import("../demo/deck.js");
  const d = new Deck();
  d.write(1, "封面", '<section><h1 style="font-size:7vmin">Hi</h1></section>');
  d.write(3, "结尾", "<section>bye</section>");
  const doc = buildDeckDocument(d, 3);

  assert(doc.startsWith("<!doctype html>"), "完整文档");
  assert(doc.includes("Hi"), "嵌入第 1 页内容");
  assert(doc.includes("bye"), "嵌入第 3 页内容");
  assert(doc.includes('class="page skeleton"'), "第 2 页缺页渲染骨架");
  assert(/let cur = 3/.test(doc) || /var cur = 3/.test(doc), "初始页码嵌入");
  assert(doc.includes("deckGo"), "postMessage 翻页通道");
  assert(doc.includes("ArrowRight"), "方向键翻页");
  assert(doc.includes("<title>封面</title>"), "文档 title 取第 1 页标题");
  assert(doc.includes('id="pg"'), "页码指示元素存在");

  const empty = buildDeckDocument(new Deck(), 1);
  assert(empty.includes("暂无页面"), "空 deck 的空态文案");
}

console.log("DECK DOC PASS");
