// common.js — demo 组共享配置：API 端点/key/model 配一次，所有 demo 共用。
// 存 localStorage（持久）；介意的话清掉浏览器数据即可，key 不经过任何服务器。

const KEY = "senza-demo-config";

export function loadCfg() {
  try { return JSON.parse(localStorage.getItem(KEY)) || {}; } catch { return {}; }
}

export function saveCfg(cfg) {
  localStorage.setItem(KEY, JSON.stringify(cfg));
}

/** 由共享配置构造 EmbeddedAgent 的 provider opts（含校验）。 */
export function providerOpts(extra = {}) {
  const cfg = loadCfg();
  if (!cfg.apiKey && !(cfg.baseUrl || "").includes("localhost"))
    throw new Error("请先配置 API key（⚙ 设置，Ollama 本地端点可留空）");
  return {
    provider: "openai",
    apiKey: cfg.apiKey || "",
    baseUrl: cfg.baseUrl || undefined,
    model: cfg.model || "deepseek-chat",
    ...extra,
  };
}

/** 把一组设置控件绑定到共享配置：预填 + 变更即存。
 * fieldIds: {preset, baseurl, apikey, model} 的 DOM id */
export function bindSettings(fieldIds, onChange) {
  const cfg = loadCfg();
  const el = (k) => document.getElementById(fieldIds[k]);
  el("baseurl").value = cfg.baseUrl || "";
  el("apikey").value = cfg.apiKey || "";
  el("model").value = cfg.model || "deepseek-chat";
  el("preset").onchange = () => {
    const u = el("preset").value;
    if (u) el("baseurl").value = u;
    el("baseurl").focus();
  };
  for (const k of ["baseurl", "apikey", "model"])
    el(k).addEventListener("change", () => {
      saveCfg({
        baseUrl: el("baseurl").value.trim(),
        apiKey: el("apikey").value.trim(),
        model: el("model").value.trim() || "deepseek-chat",
      });
      onChange?.();
    });
}
