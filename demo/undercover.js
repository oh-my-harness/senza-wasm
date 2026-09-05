// undercover.js — 谁是卧底 demo 的纯逻辑模块：词库、发词、票型统计、
// 胜负判定、回合状态机。不碰 DOM/localStorage —— Node 测试直接 import
//（仿 deck.js 模式）。agent 编排（prompt 措辞、事件泵）在 undercover.html。

/** 词库：每局随机抽一对。[平民词, 卧底词] —— 两者易混淆是玩法核心。 */
export const WORD_PAIRS = [
  ["苹果", "梨子"], ["可乐", "雪碧"], ["地铁", "公交车"], ["筷子", "勺子"],
  ["洗发水", "沐浴露"], ["沙发", "躺椅"], ["猫", "狐狸"], ["饺子", "馄饨"],
  ["雨伞", "遮阳伞"], ["手机", "平板"], ["口红", "马克笔"], ["篮球", "排球"],
  ["台灯", "手电筒"], ["蚊香", "蜡烛"], ["冰激凌", "优酸乳"], ["婚礼", "葬礼"],
  ["熊猫", "小猪"], ["高铁", "飞机"], ["眉毛", "睫毛"], ["泡面", "热干面"],
  ["洗衣机", "洗碗机"], ["孙悟空", "奥特曼"], ["微博", "朋友圈"], ["键盘", "钢琴"],
];

/** 座位上的玩家。kind 区分宿主类型；isUndercover 只有发词者知道。 */
export function makePlayer(seat, name, kind) {
  return { seat, name, kind, alive: true, isUndercover: false, word: "" };
}

/**
 * 开局：随机抽词对 + 随机指定卧底，返回 { common, undercover, players }。
 * players 传入完整座位数组（本 demo 固定 4 人，但逻辑不写死）。
 * 复制词到各 player（卧底拿 undercover 词，其余拿 common）。
 */
export function dealWords(players, pair, undercoverSeat) {
  const [common, undercover] = pair;
  for (const p of players) {
    p.isUndercover = p.seat === undercoverSeat;
    p.word = p.isUndercover ? undercover : common;
  }
  return { common, undercover };
}

/**
 * 票型统计。votes: { 投票人seat: 被投seat }（调用方保证不投自己、只投存活者）。
 * 返回 { counts, maxSeats, tie, eliminated }：
 * - counts: seat → 票数（只含被投过的 seat）
 * - maxSeats: 并列最高票的 seat 数组（按 seat 升序）
 * - tie: 最高票并列人数 > 1
 * - eliminated: 唯一最高票者出局的 seat；平票为 null（无人出局，直接下一轮）
 */
export function tallyVotes(votes) {
  const counts = new Map();
  for (const target of Object.values(votes)) {
    counts.set(target, (counts.get(target) ?? 0) + 1);
  }
  if (!counts.size) return { counts, maxSeats: [], tie: false, eliminated: null };
  let max = 0;
  for (const n of counts.values()) max = Math.max(max, n);
  const maxSeats = [...counts.entries()].filter(([, n]) => n === max)
    .map(([s]) => s).sort((a, b) => a - b);
  return {
    counts, maxSeats,
    tie: maxSeats.length > 1,
    eliminated: maxSeats.length === 1 ? maxSeats[0] : null,
  };
}

/**
 * 胜负判定。aliveCount 为存活人数，undercoverAlive 为卧底是否存活。
 * 返回 null = 游戏继续；否则：
 * - { winner: "civilians", reason: "undercover_out" }   卧底被投出局
 * - { winner: "undercover", reason: "last_two" }        只剩 2 人且卧底存活
 */
export function checkGameOver(aliveCount, undercoverAlive) {
  if (!undercoverAlive) return { winner: "civilians", reason: "undercover_out" };
  if (aliveCount <= 2) return { winner: "undercover", reason: "last_two" };
  return null;
}

/**
 * 回合状态机：dealing → speaking → voting → resolving → (下一轮 dealing | gameover)。
 * 纯数据驱动：advance() 只根据当前状态和外部填充的结果字段推进，
 * 不发起任何 agent 调用 —— 编排层（HTML）负责填 speechs/votes 再调 advance。
 *
 * 字段（编排层读写）：
 * - phase: 上述五态
 * - round: 从 1 起
 * - speakIdx: speaking 阶段的座位指针（沿 aliveOrder 前进）
 * - speechs: 本轮各座位发言（seat → 文本），轮到某人前包含此前所有发言
 */
export function newRoundState(players, firstSpeaker) {
  const aliveOrder = players.filter((p) => p.alive).map((p) => p.seat);
  const idx = Math.max(0, aliveOrder.indexOf(firstSpeaker));
  return {
    phase: "dealing",
    round: 1,
    aliveOrder,
    speakIdx: idx,
    speechs: {},
    votes: {},
    speakTotal: aliveOrder.length,
  };
}

/** speaking 阶段：当前该谁说话（座位号）。 */
export function currentSpeaker(st) {
  return st.phase === "speaking" ? st.aliveOrder[st.speakIdx % st.speakTotal] : null;
}

/**
 * 记一条发言并推进指针；全员说满 → 自动转 voting。
 * 返回 true 表示轮次阶段发生变化（编排层据此收束发言 UI）。
 */
export function recordSpeech(st, seat, text) {
  st.speechs[seat] = text;
  st.speakIdx = (st.speakIdx + 1) % st.speakTotal;
  if (Object.keys(st.speechs).length >= st.speakTotal) { st.phase = "voting"; return true; }
  return false;
}

/** 记一票。返回 true 表示全员投满 → 编排层可调 tallyVotes 结算。 */
export function recordVote(st, seat, target) {
  st.votes[seat] = target;
  return Object.keys(st.votes).length >= st.speakTotal;
}
