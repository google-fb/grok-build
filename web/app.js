import { reconstructSession } from "./reconstruct.mjs";

const FORMAT = "grok-session-stats/v1";
const state = {
  stats: null,
  workflow: null,
  chosenTurn: null,
  detail: null,
  sessions: [],
  activeId: null,
  consumeUrlSelection: true,
};

function formatTokens(n) {
  const v = Number(n) || 0;
  if (v < 1000) return String(v);
  if (v < 1_000_000) {
    const k = v / 1000;
    return (k >= 100 ? Math.round(k) : Math.round(k * 10) / 10) + "K";
  }
  const m = v / 1_000_000;
  return (m >= 100 ? Math.round(m) : Math.round(m * 10) / 10) + "M";
}

function formatDuration(ms) {
  const n = Number(ms);
  if (!Number.isFinite(n)) return "—";
  if (n < 1000) return Math.round(n) + "ms";
  const s = n / 1000;
  if (s < 60) return (Math.round(s * 10) / 10) + "s";
  const whole = Math.round(s);
  return Math.floor(whole / 60) + "m" + (whole % 60) + "s";
}

function formatTime(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return "—";
  const date = new Date(value);
  return String(date.getHours()).padStart(2, "0") + ":" +
    String(date.getMinutes()).padStart(2, "0") + ":" +
    String(date.getSeconds()).padStart(2, "0");
}

function cacheHitPercent(usage) {
  const input = Number(usage.input_tokens) || 0;
  const cached = Number(usage.cached_read_tokens) || 0;
  if (input <= 0) return null;
  return Math.min(100, Math.round((cached / input) * 100));
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function encode(value, pretty) {
  try {
    return pretty ? JSON.stringify(value, null, 2) : JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function concise(value, fallback) {
  const text = String(value || "").replace(/\s+/g, " ").trim();
  return text === "" ? (fallback || "") : text;
}

function showError(msg) {
  const el = document.getElementById("err");
  el.textContent = msg;
  el.classList.add("show");
}

function clearError() {
  document.getElementById("err").classList.remove("show");
}

function parseBundle(raw) {
  let data;
  try {
    data = JSON.parse(String(raw).replace(/^\uFEFF/, ""));
  } catch {
    throw new Error("不是合法 JSON");
  }
  if (!data || typeof data !== "object") throw new Error("JSON 根必須是物件");
  if (Array.isArray(data.turns) && data.requestCount != null && !data.format) {
    return { stats: null, workflow: data };
  }
  if (data.format !== FORMAT) {
    throw new Error("需要 format: " + FORMAT + "，或 grok session jsonl");
  }
  if (typeof data.session_id !== "string" || !data.usage || typeof data.usage !== "object") {
    throw new Error("缺少 session_id 或 usage");
  }
  data.agents = Array.isArray(data.agents) ? data.agents : [];
  data.isolation = data.isolation && typeof data.isolation === "object" ? data.isolation : {};
  return { stats: data, workflow: data.workflow || null };
}

function isolationState(iso) {
  const enabled = iso.memory_enabled === true;
  const injected = iso.memory_context_injected === true;
  const tools = Number(iso.memory_tool_calls) || 0;
  const fail = enabled || injected || tools > 0;
  const parts = [
    "memory_enabled=" + (enabled ? "true" : "false"),
    "memory_context_injected=" + (injected ? "true" : "false"),
    "memory_tool_calls=" + tools,
  ];
  return { fail, text: (fail ? "記憶隔離失敗：" : "記憶隔離通過：") + parts.join(" · ") };
}

function metric(label, value) {
  return '<div class="metric"><b>' + escapeHtml(value) + "</b><span>" + escapeHtml(label) + "</span></div>";
}

function table(headers, rows) {
  if (!rows.length) return '<div class="empty">沒有列</div>';
  const head = headers.map((h) => "<th>" + escapeHtml(h) + "</th>").join("");
  const body = rows.map((r) => "<tr>" + r.map((c) => "<td>" + c + "</td>").join("") + "</tr>").join("");
  return "<table><thead><tr>" + head + "</tr></thead><tbody>" + body + "</tbody></table>";
}

function collectTools(agents) {
  const map = new Map();
  for (const agent of agents) {
    const tools = Array.isArray(agent.tools) ? agent.tools : [];
    for (const tool of tools) {
      const name = String(tool.name || "");
      const prev = map.get(name) || { name, count: 0, duration_ms: 0 };
      prev.count += Number(tool.count) || 0;
      prev.duration_ms += Number(tool.duration_ms) || 0;
      map.set(name, prev);
    }
  }
  return [...map.values()].sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
}

function statusLabel(status) {
  if (status === "complete") return "完成";
  if (status === "error") return "錯誤";
  if (status === "running") return "進行中";
  return status || "";
}

function statusClass(status) {
  if (status === "complete") return "wfjson-ok";
  if (status === "error") return "wfjson-err";
  if (status === "running") return "wfjson-run";
  return "";
}

function jsonLeaf(value) {
  if (value === null) return '<span class="wfjson-kw">null</span>';
  if (typeof value === "boolean") return '<span class="wfjson-kw">' + value + "</span>";
  if (typeof value === "number") return '<span class="wfjson-n">' + escapeHtml(String(value)) + "</span>";
  if (typeof value === "string") return '<span class="wfjson-s">' + escapeHtml(JSON.stringify(value)) + "</span>";
  return escapeHtml(String(value));
}

function collapsedHint(value) {
  if (Array.isArray(value)) return value.length + " items";
  if (value && typeof value === "object") return Object.keys(value).length + " keys";
  return "";
}

function renderTree(name, value, depth) {
  const isRoot = name == null;
  const keyHtml = isRoot ? "" : '<span class="wfjson-k">' + escapeHtml(JSON.stringify(String(name))) + ": </span>";
  const compound = value && typeof value === "object";
  if (!compound) {
    return '<div class="wfjson-row">' + keyHtml + jsonLeaf(value) + "</div>";
  }
  const isArr = Array.isArray(value);
  const keys = isArr ? value.map((_, i) => i) : Object.keys(value);
  const open = isArr ? "[" : "{";
  const close = isArr ? "]" : "}";
  const id = "n" + Math.random().toString(36).slice(2);
  if (keys.length === 0) {
    return '<div class="wfjson-row">' + keyHtml + open + " " + close + "</div>";
  }
  const kids = keys.map((k) => renderTree(isArr ? k : k, isArr ? value[k] : value[k], depth + 1)).join("");
  return (
    '<div class="wfjson-row" data-tree="' + id + '">' +
      '<span class="wfjson-tog" data-toggle="' + id + '">▾</span>' +
      keyHtml +
      '<span class="wfjson-open">' + open + "</span>" +
      '<span class="wfjson-ellipsis hidden" data-ellipsis="' + id + '">' + open + "…" + close + " " + collapsedHint(value) + "</span>" +
      '<div class="wfjson-kids" data-kids="' + id + '">' + kids + "</div>" +
      '<div class="wfjson-close">' + close + "</div>" +
    "</div>"
  );
}

function inspectorHtml(data, paneId) {
  const pretty = encode(data, true);
  const compact = encode(data, false);
  return (
    '<div class="wfjson-insp" data-insp="' + paneId + '">' +
      '<div class="wfjson-bar">' +
        '<span class="wfjson-lang">{ } JSON</span>' +
        '<div class="wfjson-modes">' +
          '<button type="button" class="wfjson-mode wfjson-modeOn" data-mode="tree">樹狀</button>' +
          '<button type="button" class="wfjson-mode" data-mode="pretty">格式化</button>' +
          '<button type="button" class="wfjson-mode" data-mode="compact">緊湊</button>' +
          '<button type="button" class="wfjson-mode" data-wide="1" title="放大">↗</button>' +
        "</div>" +
      "</div>" +
      '<div class="wfjson-view" data-view>' +
        '<div class="wfjson-tree" data-pane="tree">' + renderTree(null, data, 0) + "</div>" +
        '<pre class="wfjson-pretty hidden" data-pane="pretty">' + escapeHtml(pretty) + "</pre>" +
        '<pre class="wfjson-pretty hidden" data-pane="compact">' + escapeHtml(compact) + "</pre>" +
      "</div>" +
    "</div>"
  );
}

function bindInspector(root) {
  root.querySelectorAll("[data-insp]").forEach((insp) => {
    const view = insp.querySelector("[data-view]");
    insp.addEventListener("click", (event) => {
      const tog = event.target.closest("[data-toggle]");
      if (tog) {
        const id = tog.getAttribute("data-toggle");
        const kids = insp.querySelector('[data-kids="' + id + '"]');
        const ell = insp.querySelector('[data-ellipsis="' + id + '"]');
        const close = tog.parentElement && tog.parentElement.querySelector(".wfjson-close");
        const open = tog.parentElement && tog.parentElement.querySelector(".wfjson-open");
        const collapsed = kids && kids.classList.toggle("hidden");
        tog.textContent = collapsed ? "▸" : "▾";
        if (ell) ell.classList.toggle("hidden", !collapsed);
        if (close) close.classList.toggle("hidden", Boolean(collapsed));
        if (open) open.classList.toggle("hidden", Boolean(collapsed));
        return;
      }
      const ellipsis = event.target.closest("[data-ellipsis]");
      if (ellipsis) {
        const id = ellipsis.getAttribute("data-ellipsis");
        const togBtn = insp.querySelector('[data-toggle="' + id + '"]');
        if (togBtn) togBtn.click();
        return;
      }
      const modeBtn = event.target.closest("[data-mode]");
      if (modeBtn) {
        const mode = modeBtn.getAttribute("data-mode");
        insp.querySelectorAll("[data-mode]").forEach((b) => b.classList.toggle("wfjson-modeOn", b === modeBtn));
        insp.querySelectorAll("[data-pane]").forEach((p) => {
          p.classList.toggle("hidden", p.getAttribute("data-pane") !== mode);
        });
        return;
      }
      const wideBtn = event.target.closest("[data-wide]");
      if (wideBtn && view) {
        const on = view.classList.toggle("wfjson-viewWide");
        wideBtn.classList.toggle("wfjson-modeOn", on);
        wideBtn.textContent = on ? "↓" : "↗";
      }
    });
  });
}

function renderStats(data) {
  const usage = data.usage;
  const iso = isolationState(data.isolation || {});
  const isoEl = document.getElementById("iso");
  isoEl.className = "banner show " + (iso.fail ? "bad" : "ok");
  isoEl.textContent = iso.text;

  const inc = document.getElementById("incomplete");
  if (usage && usage.usage_is_incomplete) {
    inc.className = "banner show warn";
    inc.textContent = "usage_is_incomplete：合計可能偏低（子 agent 尚未 fold 或 drain 超時）";
  } else {
    inc.className = "banner";
    inc.textContent = "";
  }

  const hit = usage ? cacheHitPercent(usage) : null;
  document.getElementById("meta").textContent =
    "session " + (data.session_id || "—") + " · exported " + (data.exported_at || "—") +
    (data.source === "session-jsonl" ? " · 來源 jsonl" : "");
  document.getElementById("metrics").innerHTML = [
    metric("輸入 tok（根 ledger）", formatTokens(usage && usage.input_tokens)),
    metric("輸出 tok", formatTokens(usage && usage.output_tokens)),
    metric("快取命中", hit === null ? "—" : hit + "%"),
    metric("模型呼叫", String((usage && usage.model_calls) || 0)),
    metric("API 時間", formatDuration(usage && usage.api_duration_ms)),
    metric("工具次數", String((data.agents || []).reduce((n, a) => n + (Number(a.tool_call_count) || 0), 0))),
  ].join("");

  const agentRows = (data.agents || []).map((a) => [
    escapeHtml(a.kind || "—"),
    "<code>" + escapeHtml(a.session_id || "") + "</code>",
    escapeHtml(a.subagent_type || "—"),
    escapeHtml(formatTokens(a.usage && a.usage.input_tokens)),
    escapeHtml(formatTokens(a.usage && a.usage.output_tokens)),
    escapeHtml(String(a.tool_call_count || 0)),
  ]);
  document.getElementById("agents").innerHTML = table(
    ["kind", "session_id", "type", "in", "out", "tools"],
    agentRows,
  );

  const toolRows = collectTools(data.agents || []).map((t) => [
    escapeHtml(t.name),
    escapeHtml(String(t.count)),
    escapeHtml(formatDuration(t.duration_ms)),
  ]);
  document.getElementById("tools").innerHTML = table(["name", "count", "duration"], toolRows);

  const models = usage && usage.by_model && typeof usage.by_model === "object" ? usage.by_model : {};
  const modelRows = Object.keys(models).map((id) => {
    const m = models[id] || {};
    return [
      escapeHtml(id),
      escapeHtml(formatTokens(m.input_tokens)),
      escapeHtml(formatTokens(m.output_tokens)),
      escapeHtml(String(m.model_calls || 0)),
    ];
  });
  document.getElementById("models").innerHTML = table(["model", "in", "out", "calls"], modelRows);
}

function svg(d) {
  return '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="' + d + '"></path></svg>';
}

function cardHtml(opts) {
  return (
    '<button type="button" class="wfjson-card ' + opts.className + (opts.selected ? " wfjson-cardOn" : "") + '" data-kind="' + opts.kind + '"' +
      (opts.tool != null ? ' data-tool="' + opts.tool + '"' : "") + ">" +
      '<span class="wfjson-cardHead"><span class="wfjson-cardTitle">' + opts.icon + opts.title + "</span></span>" +
      '<span class="wfjson-cardBody">' +
        '<span class="wfjson-cardPreview">' + escapeHtml(opts.preview) + "</span>" +
        (opts.result || ('<span class="wfjson-chips">' + opts.chips.map((c) => "<span>" + escapeHtml(c) + "</span>").join("") + "</span>")) +
      "</span>" +
    "</button>"
  );
}

function detailHtml(call, target) {
  let title = "請求詳情";
  let panes = [];
  if (target.kind === "request") {
    panes = [
      { label: "系統提示詞", data: { system: call.system || "" } },
      { label: "消息", data: { messages: call.messages || [] } },
      { label: "工具定義", data: { tools: call.toolDefs || [] } },
    ];
  } else if (target.kind === "response") {
    title = "響應詳情";
    const response = call.response || {};
    panes = [
      { label: "推理", data: { reasoning: response.reasoning || "" } },
      { label: "內容", data: { content: response.content || "", tool_calls: response.tool_calls || [] } },
      { label: "原始 JSON", data: response },
    ];
  } else {
    title = "工具詳情";
    const tool = (call.tools || [])[target.tool] || {};
    panes = [
      { label: "參數", data: { arguments: tool.arguments } },
      { label: "結果", data: { result: tool.result, error: tool.error } },
      { label: "原始 JSON", data: tool },
    ];
  }
  return (
    '<section class="wfjson-detail">' +
      "<header><strong>" + title + '</strong><button type="button" data-close-detail aria-label="收起">⌃</button></header>' +
      '<div class="wfjson-grid">' +
        panes.map((pane, i) => (
          '<section class="wfjson-col"><h4>' + escapeHtml(pane.label) + "</h4>" + inspectorHtml(pane.data, target.kind + i) + "</section>"
        )).join("") +
      "</div>" +
    "</section>"
  );
}

function callRowHtml(call, detail) {
  const selectedKey = detail ? (detail.kind === "tool" ? "tool:" + detail.tool : detail.kind) : "";
  const usage = call.usage || {};
  const response = call.response || {};
  const input = Number(usage.inputTokens) || 0;
  const cached = Number(usage.cacheReadTokens) || 0;
  const uncached = Math.max(0, input - cached);
  const chips = [];
  if (usage.inputTokens != null && usage.inputTokens !== undefined && Object.keys(usage).length) {
    chips.push(["輸入 " + input.toLocaleString(), true]);
    chips.push(["未緩存 " + uncached.toLocaleString(), true]);
    chips.push(["緩存命中 " + cached.toLocaleString(), true]);
    chips.push(["輸出 " + (Number(usage.outputTokens) || 0).toLocaleString(), true]);
  }
  const toolCards = (call.tools || []).map((tool, index) => {
    const ok = tool.status === "complete";
    const bad = tool.status === "error";
    const preview = concise(
      typeof tool.arguments === "string" ? tool.arguments : encode(tool.arguments, false),
      tool.callId,
    );
    const resultPreview = concise(
      typeof tool.result === "string" ? tool.result : encode(tool.result, false),
      tool.callId || "",
    );
    return (
      '<span style="display:flex;align-items:stretch">' +
        '<span class="wfjson-arrow">→</span>' +
        cardHtml({
          className: bad ? "wfjson-tool wfjson-toolErr" : "wfjson-tool",
          selected: selectedKey === "tool:" + index,
          kind: "tool",
          tool: index,
          icon: svg("M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"),
          title: escapeHtml(tool.name || "工具"),
          preview,
          chips: [],
          result:
            '<span class="wfjson-toolRes ' + (bad ? "wfjson-toolBad" : ok ? "wfjson-toolOk" : "") + '">' +
              "<span>" + escapeHtml(statusLabel(tool.status)) + "</span>" +
              "<span>" + escapeHtml(resultPreview) + "</span>" +
              "<time>" + escapeHtml(formatDuration(tool.durationMs)) + "</time>" +
            "</span>",
        }) +
      "</span>"
    );
  }).join("");

  return (
    '<article class="wfjson-call" data-call="' + escapeHtml(call.id) + '">' +
      '<header class="wfjson-callHead">' +
        "<strong>模型調用 #" + call.number + "</strong>" +
        "<span>" + formatTime(call.startedAt) + "</span>" +
        "<span>" + formatDuration(call.durationMs) + "</span>" +
        chips.map((c) => "<span>" + c[0] + "</span>").join("") +
        '<span class="' + statusClass(call.status) + '">' + statusLabel(call.status) + "</span>" +
      "</header>" +
      '<div class="wfjson-flow">' +
        cardHtml({
          className: "wfjson-request",
          selected: selectedKey === "request",
          kind: "request",
          icon: svg("M22 2L11 13M22 2l-7 20-4-9-9-4 20-7z"),
          title: "REQUEST 請求",
          preview: call.model || "請求上下文",
          chips: [
            "System " + (call.system ? 1 : 0),
            "消息 " + ((call.messages || []).length),
            "工具定義 未記錄",
          ],
        }) +
        '<span class="wfjson-arrow">→</span>' +
        cardHtml({
          className: "wfjson-response",
          selected: selectedKey === "response",
          kind: "response",
          icon: svg("M12 5a7 7 0 0 1 7 7v1H5v-1a7 7 0 0 1 7-7zm-4 13h8"),
          title: "RESPONSE 響應",
          preview: concise(response.content, response.reasoning || ((response.tool_calls || []).length ? "僅工具調用" : "待響應")),
          chips: [
            "Reasoning " + (response.reasoning ? 1 : 0),
            "Content " + (response.content ? 1 : 0),
            "工具調用 " + ((response.tool_calls || call.tools || []).length),
          ],
        }) +
        toolCards +
      "</div>" +
      (detail && detail.id === call.id ? detailHtml(call, detail) : "") +
    "</article>"
  );
}

function renderWorkflow() {
  const host = document.getElementById("workflow");
  const wf = state.workflow;
  if (!wf || !Array.isArray(wf.turns) || wf.turns.length === 0) {
    host.innerHTML = '<div class="wfjson-root"><div class="wfjson-empty">沒有從 jsonl 還原出模型調用。請用 node serve.mjs 開這個頁，或拖入 session 資料夾。</div></div>';
    return;
  }
  const turns = wf.turns;
  let selected = turns.find((t) => t.turn === state.chosenTurn) || turns[turns.length - 1];
  const calls = selected.calls || [];
  host.innerHTML =
    '<div class="wfjson-root">' +
      '<header class="wfjson-summary">' +
        '<div class="wfjson-summaryTitle"><strong>工作流 JSON</strong></div>' +
        '<div class="wfjson-metrics">' +
          '<span class="wfjson-metric"><strong>' + turns.length + "</strong><span>用戶對話</span></span>" +
          '<span class="wfjson-metric"><strong>' + (wf.requestCount || 0) + "</strong><span>模型調用</span></span>" +
          '<span class="wfjson-metric"><strong>' + (wf.toolCount || 0) + "</strong><span>工具調用</span></span>" +
          '<span class="wfjson-metric"><strong>' + formatDuration(wf.durationMs) + "</strong><span>總耗時</span></span>" +
        "</div>" +
      "</header>" +
      '<div class="wfjson-workspace">' +
        '<aside class="wfjson-turns">' +
          "<header><strong>用戶對話</strong><span>" + turns.length + "</span></header>" +
          '<div class="wfjson-turnList">' +
            turns.map((turn) => (
              '<button type="button" class="wfjson-turn' + (selected.turn === turn.turn ? " wfjson-turnSelected" : "") + '" data-turn="' + turn.turn + '">' +
                '<span class="wfjson-turnTop"><strong>第' + turn.turn + "輪：" + escapeHtml(turn.promptPreview || "") + "</strong><time>" + formatTime(turn.startedAt) + "</time></span>" +
                '<span class="wfjson-turnBottom"><span>' + turn.calls.length + "次模型調用 · " + turn.toolCount + "次工具</span>" +
                '<span class="' + statusClass(turn.status) + '">' + statusLabel(turn.status) + "</span></span>" +
              "</button>"
            )).join("") +
          "</div>" +
        "</aside>" +
        '<main class="wfjson-main">' +
          '<header class="wfjson-turnHeader">' +
            '<strong title="' + escapeHtml(selected.prompt || "") + '">第' + selected.turn + "輪：" + escapeHtml(selected.promptPreview || "") + "</strong>" +
            '<div class="wfjson-turnMeta">' +
              "<span>" + formatTime(selected.startedAt) + "</span>" +
              "<span>" + formatDuration(selected.durationMs) + "</span>" +
              "<span>" + calls.length + "次模型調用</span>" +
              "<span>" + selected.toolCount + "次工具</span>" +
            "</div>" +
          "</header>" +
          '<div class="wfjson-calls">' +
            (calls.length === 0
              ? '<div class="wfjson-empty">這一輪還沒有模型調用</div>'
              : calls.map((call) => callRowHtml(call, state.detail)).join("")) +
          "</div>" +
        "</main>" +
      "</div>" +
    "</div>";

  bindInspector(host);
  host.querySelectorAll("[data-turn]").forEach((btn) => {
    btn.addEventListener("click", () => {
      state.chosenTurn = Number(btn.getAttribute("data-turn"));
      state.detail = null;
      renderWorkflow();
    });
  });
  host.querySelectorAll(".wfjson-card").forEach((btn) => {
    btn.addEventListener("click", () => {
      const article = btn.closest("[data-call]");
      const id = article && article.getAttribute("data-call");
      const kind = btn.getAttribute("data-kind");
      const tool = btn.hasAttribute("data-tool") ? Number(btn.getAttribute("data-tool")) : undefined;
      const next = { id, kind, tool };
      const same = state.detail && state.detail.id === id && state.detail.kind === kind && state.detail.tool === tool;
      state.detail = same ? null : next;
      renderWorkflow();
    });
  });
  host.querySelectorAll("[data-close-detail]").forEach((btn) => {
    btn.addEventListener("click", () => {
      state.detail = null;
      renderWorkflow();
    });
  });
}

function renderAll() {
  document.getElementById("view").classList.remove("hidden");
  if (state.stats) renderStats(state.stats);
  renderWorkflow();
}

function applyUrlSelection() {
  const params = new URLSearchParams(location.search);
  const turnRaw = params.get("turn");
  const open = params.get("open");
  if (turnRaw) state.chosenTurn = Number(turnRaw);
  const wf = state.workflow;
  if (!wf || !Array.isArray(wf.turns) || !wf.turns.length) return;
  const selected = wf.turns.find((t) => t.turn === state.chosenTurn) || wf.turns[wf.turns.length - 1];
  if (!open || !selected) return;
  const call = (open === "tool"
    ? selected.calls.find((c) => (c.tools || []).length)
    : selected.calls[0]) || selected.calls[0];
  if (!call) return;
  state.detail = {
    id: call.id,
    kind: open === "tool" || open === "request" || open === "response" ? open : "request",
    tool: open === "tool" ? 0 : undefined,
  };
}

function applyLoaded(loaded, activeId) {
  clearError();
  state.stats = loaded.stats || null;
  state.workflow = loaded.workflow || null;
  state.chosenTurn = null;
  state.detail = null;
  state.activeId = activeId || (loaded.stats && loaded.stats.session_id) || null;
  if (state.consumeUrlSelection) {
    applyUrlSelection();
    state.consumeUrlSelection = false;
  }
  if (!state.stats && !state.workflow) throw new Error("沒有統計也沒有 jsonl");
  if (!state.stats && state.workflow) {
    state.stats = {
      format: FORMAT,
      session_id: state.activeId || "",
      usage: { input_tokens: 0, output_tokens: 0, cached_read_tokens: 0, model_calls: state.workflow.requestCount, api_duration_ms: 0, by_model: {} },
      isolation: {},
      agents: [],
      source: "session-jsonl",
    };
  }
  renderAll();
  renderSessionList();
}

async function fetchSession(id) {
  const res = await fetch("/api/session/" + encodeURIComponent(id));
  const data = await res.json();
  if (!res.ok) throw new Error(data.error || ("HTTP " + res.status));
  applyLoaded(data, id);
}

async function loadSessions() {
  try {
    const res = await fetch("/api/sessions");
    if (!res.ok) throw new Error("HTTP " + res.status);
    const data = await res.json();
    state.sessions = data.sessions || [];
    renderSessionList();
    return true;
  } catch {
    document.getElementById("sessionBox").classList.add("hidden");
    return false;
  }
}

function renderSessionList() {
  const box = document.getElementById("sessionBox");
  const host = document.getElementById("sessions");
  if (!state.sessions.length) {
    box.classList.add("hidden");
    return;
  }
  box.classList.remove("hidden");
  host.innerHTML = state.sessions.map((s) => (
    '<button type="button" class="sess' + (s.session_id === state.activeId ? " on" : "") + '" data-sid="' + escapeHtml(s.session_id) + '">' +
      "<b>" + escapeHtml(s.title || s.session_id) + "</b>" +
      "<small><code>" + escapeHtml(s.session_id) + "</code> · " + escapeHtml(s.cwd || "") + "</small>" +
    "</button>"
  )).join("");
  host.querySelectorAll("[data-sid]").forEach((btn) => {
    btn.addEventListener("click", () => {
      fetchSession(btn.getAttribute("data-sid")).catch((e) => showError(e.message || String(e)));
    });
  });
}

function filesFromList(fileList) {
  const files = [...fileList];
  const byName = new Map();
  for (const file of files) {
    const rel = file.webkitRelativePath || file.name;
    const base = rel.split(/[/\\]/).pop().toLowerCase();
    byName.set(base, file);
  }
  return { files, byName };
}

async function loadDropped(fileList) {
  const { files, byName } = filesFromList(fileList);
  const history = byName.get("chat_history.jsonl");
  const updates = byName.get("updates.jsonl");
  if (history || updates) {
    const loaded = reconstructSession({
      chatHistory: history ? await history.text() : "",
      updates: updates ? await updates.text() : "",
      usageJson: byName.get("usage.json") ? await byName.get("usage.json").text() : "",
      summaryJson: byName.get("summary.json") ? await byName.get("summary.json").text() : "",
      promptContextJson: byName.get("prompt_context.json") ? await byName.get("prompt_context.json").text() : "",
      statsJson: byName.get("sample.json") || byName.has("export.json") ? "" : "",
    });
    applyLoaded(loaded, loaded.stats && loaded.stats.session_id);
    return;
  }
  const jsonFile = files.find((f) => /\.json$/i.test(f.name));
  if (!jsonFile) throw new Error("請拖 session 資料夾、chat_history.jsonl，或 grok-session-stats/v1 JSON");
  const parsed = parseBundle(await jsonFile.text());
  if (parsed.stats && parsed.stats.session_id) {
    try {
      await fetchSession(parsed.stats.session_id);
      return;
    } catch (err) {
      applyLoaded(parsed, parsed.stats.session_id);
      showError("統計已載入，但自動讀 jsonl 失敗：" + (err.message || String(err)) + "。用 node serve.mjs 開這個頁，或改拖 session 資料夾。");
      document.getElementById("err").classList.add("show");
      return;
    }
  }
  applyLoaded(parsed);
}

const drop = document.getElementById("drop");
drop.addEventListener("click", () => document.getElementById("file").click());
drop.addEventListener("dragover", (e) => { e.preventDefault(); drop.classList.add("over"); });
drop.addEventListener("dragleave", () => drop.classList.remove("over"));
drop.addEventListener("drop", (e) => {
  e.preventDefault();
  drop.classList.remove("over");
  const list = e.dataTransfer && e.dataTransfer.files;
  if (list && list.length) {
    loadDropped(list).catch((err) => showError(err.message || String(err)));
  }
});
document.getElementById("file").addEventListener("change", (e) => {
  const list = e.target.files;
  if (list && list.length) loadDropped(list).catch((err) => showError(err.message || String(err)));
});
document.getElementById("dir").addEventListener("change", (e) => {
  const list = e.target.files;
  if (list && list.length) loadDropped(list).catch((err) => showError(err.message || String(err)));
});
document.addEventListener("paste", (e) => {
  const text = e.clipboardData && e.clipboardData.getData("text");
  if (text && text.trim().startsWith("{")) {
    try {
      applyLoaded(parseBundle(text));
    } catch (err) {
      showError(err.message || String(err));
    }
  }
});
document.getElementById("sample").addEventListener("click", async () => {
  try {
    const res = await fetch("sample.json");
    if (!res.ok) throw new Error("HTTP " + res.status);
    const parsed = parseBundle(await res.text());
    if (parsed.stats && parsed.stats.session_id) {
      try {
        await fetchSession(parsed.stats.session_id);
        return;
      } catch {
        applyLoaded(parsed);
        showError("sample.json 只有彙總。請用 node serve.mjs 開頁，會自動讀同一個 session 的 jsonl。");
        return;
      }
    }
    applyLoaded(parsed);
  } catch {
    showError("無法載入 sample.json。請用 node serve.mjs 開這個頁。");
  }
});
document.getElementById("reload").addEventListener("click", () => {
  loadSessions().then((ok) => {
    if (!ok) showError("掃描失敗。請在檢視器目錄執行 node serve.mjs。");
  });
});

loadSessions().then((ok) => {
  if (!ok) return;
  const want = new URLSearchParams(location.search).get("session");
  const id = want || (state.sessions[0] && state.sessions[0].session_id);
  if (id) fetchSession(id).catch((e) => showError(e.message || String(e)));
});
