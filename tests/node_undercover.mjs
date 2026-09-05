import assert from "node:assert";
import { EmbeddedAgent } from "../pkg-node/senza_wasm.js";
import {
  WORD_PAIRS, makePlayer, dealWords, tallyVotes, checkGameOver,
  newRoundState, currentSpeaker, recordSpeech, recordVote,
} from "../demo/undercover.js";

// —— 1. 词库 & 发词 ——
{
  assert.ok(WORD_PAIRS.length >= 20, "词库至少 20 对");
  for (const [a, b] of WORD_PAIRS) {
    assert.ok(typeof a === "string" && a && typeof b === "string" && b, `词对完整: ${a}/${b}`);
    assert.notStrictEqual(a, b, `词对不相等: ${a}`);
  }
  const players = [0, 1, 2, 3].map((s) => makePlayer(s, `P${s}`, s === 3 ? "human" : "agent"));
  const { common, undercover } = dealWords(players, ["苹果", "梨子"], 1);
  assert.strictEqual(common, "苹果");
  assert.strictEqual(undercover, "梨子");
  assert.strictEqual(players[1].word, "梨子", "卧底拿卧底词");
  assert.strictEqual(players[0].word, "苹果", "平民拿平民词");
  assert.ok(players.filter((p) => p.isUndercover).length === 1, "恰好一个卧底");
}

// —— 2. 票型统计 ——
{
  const r1 = tallyVotes({ 0: 2, 1: 2, 2: 1, 3: 2 });
  assert.strictEqual(r1.eliminated, 2, "唯一最高票出局");
  assert.strictEqual(r1.tie, false);
  assert.deepStrictEqual(r1.maxSeats, [2]);

  const r2 = tallyVotes({ 0: 1, 1: 2, 2: 1, 3: 2 });
  assert.strictEqual(r2.eliminated, null, "平票无人出局");
  assert.strictEqual(r2.tie, true);
  assert.deepStrictEqual(r2.maxSeats, [1, 2]);

  const r3 = tallyVotes({ 0: 1 });
  assert.strictEqual(r3.eliminated, 1, "单人投票也结算");
  const r4 = tallyVotes({});
  assert.strictEqual(r4.eliminated, null, "空票型");
}

// —— 3. 胜负判定 ——
{
  assert.deepStrictEqual(checkGameOver(3, false), { winner: "civilians", reason: "undercover_out" });
  assert.deepStrictEqual(checkGameOver(2, true), { winner: "undercover", reason: "last_two" });
  assert.strictEqual(checkGameOver(3, true), null, "3 人卧底存活继续");
  assert.strictEqual(checkGameOver(4, true), null, "4 人继续");
}

// —— 4. 回合状态机 ——
{
  const players = [0, 1, 2, 3].map((s) => makePlayer(s, `P${s}`, "agent"));
  players[2].alive = false; // 模拟第 2 轮：3 座位存活
  const st = newRoundState(players, 3);
  assert.deepStrictEqual(st.aliveOrder, [0, 1, 3], "只含存活座位");
  assert.strictEqual(st.speakTotal, 3);
  st.phase = "speaking"; // 编排层发完词后置位

  assert.strictEqual(currentSpeaker(st), 3, "从指定首发言人起");
  assert.strictEqual(recordSpeech(st, 3, "我的是水果"), false);
  assert.strictEqual(currentSpeaker(st), 0);
  assert.strictEqual(recordSpeech(st, 0, "我见过它"), false);
  assert.strictEqual(recordSpeech(st, 1, "很常见"), true, "全员说满 → 阶段翻转");
  assert.strictEqual(st.phase, "voting");

  assert.strictEqual(recordVote(st, 3, 1), false);
  assert.strictEqual(recordVote(st, 0, 1), false);
  assert.strictEqual(recordVote(st, 1, 1), true, "全员投满");
  const r = tallyVotes(st.votes);
  assert.strictEqual(r.eliminated, 1, "3 票齐投 1 号出局");
}

// —— 5. 工具闭包全链路（mock provider）：发言必须走 speak 工具 ——
{
  const speakResults = [];
  const agent = new EmbeddedAgent(JSON.stringify({
    provider: "mock", model: "mock-model",
    mockScript: [
      { kind: "tool_use", toolUseId: "t1", name: "speak",
        args: JSON.stringify({ statement: "它的皮是红色的" }) },
      { kind: "text", text: "done" },
    ],
  }));
  agent.registerTool("speak", "say your statement", JSON.stringify(
    { type: "object", properties: { statement: { type: "string" } }, required: ["statement"] }),
    (argsJson) => {
      const s = JSON.parse(argsJson).statement;
      speakResults.push(s);
      return JSON.stringify({ text: `已记录发言：${s}` });
    });

  // speak 工具结果 = demo 编排层唯一采信的发言通道（text 轮只是模型的自言自语）
  agent.prompt("请发言");
  for (let i = 0; i < 400; i++) {
    await new Promise((r) => setTimeout(r, 25));
    let end = false;
    for (const l of agent.poll()) {
      const ev = JSON.parse(l);
      if (ev.type === "agent_end") end = true;
    }
    if (end) break;
  }
  assert.deepStrictEqual(speakResults, ["它的皮是红色的"],
    "发言经工具结果回传: " + JSON.stringify(speakResults));
}

// —— 6. 多轮 session：发言 → 投票 两轮工具调用，同一 agent 续接 ——
{
  const acts = [];
  const agent = new EmbeddedAgent(JSON.stringify({
    provider: "mock", model: "mock-model",
    mockScript: [
      { kind: "tool_use", toolUseId: "t1", name: "speak", args: JSON.stringify({ statement: "我的是水果" }) },
      { kind: "text", text: "ok" },
      { kind: "tool_use", toolUseId: "t2", name: "vote", args: JSON.stringify({ target: "P2" }) },
      { kind: "text", text: "ok" },
    ],
  }));
  agent.registerTool("speak", "say statement", JSON.stringify(
    { type: "object", properties: { statement: { type: "string" } }, required: ["statement"] }),
    (a) => { const s = JSON.parse(a).statement;
      acts.push("speak:" + s); return JSON.stringify({ text: "已记录" }); });
  agent.registerTool("vote", "vote a player", JSON.stringify(
    { type: "object", properties: { target: { type: "string" } }, required: ["target"] }),
    (a) => { acts.push("vote:" + JSON.parse(a).target); return JSON.stringify({ text: "已投票" }); });

  const run = async (p) => {
    agent.prompt(p);
    for (let i = 0; i < 400; i++) {
      await new Promise((r) => setTimeout(r, 25));
      let end = false;
      for (const l of agent.poll()) { if (JSON.parse(l).type === "agent_end") end = true; }
      if (end) break;
    }
  };
  await run("轮到你了，请发言");
  await run("投票环节，请投票");
  assert.deepStrictEqual(acts, ["speak:我的是水果", "vote:P2"],
    "同一 session 两轮分别走 speak/vote: " + JSON.stringify(acts));
  const cost = JSON.parse(agent.costSnapshot());
  assert.strictEqual(cost.providerCalls, 4, "两轮 = 4 次 provider 调用（每轮 2）");
}

console.log("UNDERCOVER LOGIC PASS");
console.log("UNDERCOVER AGENT SMOKE PASS");
