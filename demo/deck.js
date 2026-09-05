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
