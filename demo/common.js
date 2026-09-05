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
    baseUrl: cfg.baseUrl || "https://api.deepseek.com/v1",  // 犯懒默认：DeepSeek
    model: cfg.model || "deepseek-v4-flash",
    ...extra,
  };
}

/** 把一组设置控件绑定到共享配置：预填 + 变更即存。
 * fieldIds: {preset, baseurl, apikey, model} 的 DOM id */
export function bindSettings(fieldIds, onChange) {
  const cfg = loadCfg();
  const el = (k) => document.getElementById(fieldIds[k]);
  el("baseurl").value = cfg.baseUrl || "https://api.deepseek.com/v1";
  el("apikey").value = cfg.apiKey || "";
  // 旧版本存过的过时默认模型 → 视为未配置，回落新默认
  const STALE = new Set(["gpt-4o-mini", "deepseek-chat"]);
  el("model").value = cfg.model && !STALE.has(cfg.model) ? cfg.model : "deepseek-v4-flash";
  // preset → 联动 base url + 该端点的默认模型
  const PRESET_DEFAULT_MODEL = {
    "https://api.anthropic.com": "claude-sonnet-4-5",
    "https://api.deepseek.com/v1": "deepseek-v4-flash",
    "https://api.openai.com/v1": "gpt-4o-mini",
  };
  el("preset").onchange = () => {
    const u = el("preset").value;
    if (u) {
      el("baseurl").value = u;
      if (PRESET_DEFAULT_MODEL[u]) el("model").value = PRESET_DEFAULT_MODEL[u];
    }
    el("baseurl").focus();
  };
  const persist = () => {
    saveCfg({
      provider: (el("baseurl").value || "").includes("anthropic") ? "anthropic" : "openai",
      baseUrl: el("baseurl").value.trim(),
      apiKey: el("apikey").value.trim(),
      model: el("model").value.trim() || "deepseek-v4-flash",
    });
    onChange?.();
  };
  for (const k of ["baseurl", "apikey", "model"])
    el(k).addEventListener("change", persist);
}
