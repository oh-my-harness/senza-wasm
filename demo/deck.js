// deck.js — Deck 状态模型 + 工具闭包工厂 + 自包含文档组装器。
// 纯模块：不 import common.js、不触碰 document/localStorage —— Node 测试直接 import。
// 持久化由宿主（deck.html）用 deck.sorted() / new Deck(entries) 完成。

export class Deck {
  /** entries: [[index, {title, html}], ...]（localStorage 恢复用） */
  constructor(entries) {
    this.slides = new Map(entries ?? []);
  }

  /** upsert：同 index 覆盖。 */
  write(index, title, html) {
    this.slides.set(index, { title, html });
  }

  /** 删页。成功返回 null；失败返回回给 LLM 的纠错文案。 */
  remove(index) {
    if (!this.slides.has(index)) return `ERROR: 第 ${index} 页不存在，未删除。`;
    this.slides.delete(index);
    return null;
  }

  maxIndex() {
    return this.slides.size ? Math.max(...this.slides.keys()) : 0;
  }

  /** 按页码排序的 [index, slide] 数组（持久化/组装用）。 */
  sorted() {
    return [...this.slides.entries()].sort((a, b) => a[0] - b[0]);
  }

  /** 大纲文本：回显进每个工具结果，模型的全局结构感。 */
  outline() {
    if (!this.slides.size) return "（空）";
    return this.sorted().map(([i, s]) => `${i}. ${s.title}`).join("\n");
  }
}

/**
 * 工具闭包工厂：writeSlide（upsert）/ deleteSlide。
 * hooks: { onWrite?(index), onDelete?(index) } —— 宿主的渲染入口。
 * 每个工具结果都带当前大纲（设计决策 1）。
 */
export function makeDeckTools(deck, hooks = {}) {
  const withOutline = (msg) =>
    JSON.stringify({ text: `${msg}\n\n当前大纲：\n${deck.outline()}` });

  return {
    register(agent) {
      agent.registerTool(
        "writeSlide", "写入（新增或覆盖）一页幻灯片",
        JSON.stringify({
          type: "object",
          properties: {
            index: { type: "integer", minimum: 1, description: "页码，从 1 开始" },
            title: { type: "string", description: "大纲里显示的页标题" },
            html: { type: "string", description: "单页内容：<section> 片段，内联样式" },
          },
          required: ["index", "title", "html"],
        }),
        async (argsJson) => {
          const { index, title, html } = JSON.parse(argsJson);
          deck.write(index, title, html);
          hooks.onWrite?.(index);
          return withOutline(`已写入第 ${index} 页「${title}」。`);
        });

      agent.registerTool(
        "deleteSlide", "删除一页幻灯片",
        JSON.stringify({
          type: "object",
          properties: { index: { type: "integer", minimum: 1, description: "页码" } },
          required: ["index"],
        }),
        async (argsJson) => {
          const { index } = JSON.parse(argsJson);
          const err = deck.remove(index);
          if (err) return withOutline(err);
          hooks.onDelete?.(index);
          return withOutline(`已删除第 ${index} 页。`);
        });
    },
  };
}

/**
 * 组装自包含幻灯片文档（预览 srcdoc 与下载文件同源）。
 * initialPage：初始显示页（生成期跳到刚写的页；下载固定 1）。
 * 缺失页码渲染骨架占位 —— 生成过程的进度感。
 */
export function buildDeckDocument(deck, initialPage = 1) {
  const max = deck.maxIndex();
  if (!max) {
    return `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><title>deck</title>
<style>html,body{height:100%;margin:0;background:#0d1017;color:#5b6472;
font:14px/1.6 -apple-system,"PingFang SC",sans-serif;display:grid;place-items:center}</style>
</head><body>暂无页面</body></html>`;
  }
  const first = deck.slides.get(1)?.title ?? "deck";
  let pages = "";
  for (let i = 1; i <= max; i++) {
    const s = deck.slides.get(i);
    pages += s
      ? `<div class="page" data-i="${i}"><div class="inner">${s.html}</div></div>\n`
      : `<div class="page skeleton" data-i="${i}"><div class="inner">
           <div class="bar" style="width:46%"></div><div class="bar" style="width:72%"></div>
           <div class="bar" style="width:58%"></div><div class="bar" style="width:34%"></div>
         </div></div>\n`;
  }
  return `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>${first}</title>
<style>
  * { box-sizing: border-box; margin: 0; }
  html, body { height: 100%; background: #0d1017; overflow: hidden;
    font: 3.2vmin/1.55 -apple-system, "PingFang SC", "Noto Sans SC", sans-serif; }
  .page { position: fixed; inset: 0; display: none; }
  .page.on { display: block; }
  .page .inner { width: 100%; height: 100%; padding: 8vmin; }
  .page.skeleton .inner { display: flex; flex-direction: column; gap: 3vmin;
    justify-content: center; max-width: 72vw; margin: 0 auto; }
  .bar { height: 3.6vmin; border-radius: 1.8vmin; background: #1c2230;
    animation: shimmer 1.4s ease-in-out infinite; }
  @keyframes shimmer { 50% { opacity: .45; } }
  #pg { position: fixed; right: 3vmin; bottom: 2.4vmin; font-size: 2vmin;
    color: #5b6472; user-select: none; }
  .zone { position: fixed; top: 0; bottom: 0; width: 30%; cursor: pointer; }
  #zl { left: 0; } #zr { right: 0; }
</style>
</head>
<body>
${pages}
<div class="zone" id="zl"></div><div class="zone" id="zr"></div>
<div id="pg"></div>
<script>
  var pages = document.querySelectorAll(".page");
  var cur = Math.max(1, Math.min(${initialPage}, pages.length));
  function show(n) {
    cur = Math.max(1, Math.min(pages.length, n));
    pages.forEach(function (p, i) { p.classList.toggle("on", i === cur - 1); });
    document.getElementById("pg").textContent = cur + " / " + pages.length;
  }
  document.addEventListener("keydown", function (e) {
    if (e.key === "ArrowRight" || e.key === " " || e.key === "PageDown") { e.preventDefault(); show(cur + 1); }
    if (e.key === "ArrowLeft" || e.key === "PageUp") { e.preventDefault(); show(cur - 1); }
  });
  document.getElementById("zl").onclick = function () { show(cur - 1); };
  document.getElementById("zr").onclick = function () { show(cur + 1); };
  addEventListener("message", function (e) {
    if (e.data && e.data.deckGo) show(cur + e.data.deckGo);
    if (e.data && e.data.deckSet) show(e.data.deckSet);
  });
  show(cur);
</script>
</body>
</html>`;
}
