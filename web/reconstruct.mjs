/** Rebuild DSH-style turn/call/tool JSON from a grok session directory. */

export function parseJsonl(text) {
  const rows = [];
  const lines = String(text || "").replace(/^\uFEFF/, "").split(/\r?\n/);
  for (const line of lines) {
    const s = line.trim();
    if (!s) continue;
    try {
      rows.push(JSON.parse(s));
    } catch {
      // skip a corrupt line; the rest of the session is still usable
    }
  }
  return rows;
}

function asString(value) {
  return typeof value === "string" ? value : "";
}

function parseArgs(raw) {
  if (typeof raw !== "string") return raw == null ? null : raw;
  try {
    return JSON.parse(raw);
  } catch {
    return raw;
  }
}

function textOfUser(rec) {
  const c = rec && rec.content;
  if (typeof c === "string") return c;
  if (Array.isArray(c)) {
    return c.map((b) => (b && b.type === "text" && typeof b.text === "string" ? b.text : "")).join("\n");
  }
  return "";
}

function extractUserQuery(text) {
  const m = String(text || "").match(/<user_query>\s*([\s\S]*?)\s*<\/user_query>/);
  return m ? m[1].trim() : "";
}

function previewText(value) {
  const text = String(value || "").replace(/\s+/g, " ").trim();
  const chars = Array.from(text);
  return chars.length > 20 ? chars.slice(0, 20).join("") + "…" : text;
}

function reasoningText(rec) {
  if (typeof rec.content === "string" && rec.content) return rec.content;
  const summary = rec.summary;
  if (Array.isArray(summary)) {
    return summary.map((s) => (s && typeof s.text === "string" ? s.text : "")).filter(Boolean).join("\n");
  }
  return "";
}

function metaOf(row) {
  const params = row && row.params;
  const update = params && params.update;
  return (update && update._meta) || (row && row._meta) || {};
}

function updateOf(row) {
  return row && row.params && row.params.update ? row.params.update : null;
}

function tsOf(row) {
  const meta = metaOf(row);
  if (typeof meta.agentTimestampMs === "number") return meta.agentTimestampMs;
  if (typeof row.timestamp === "number") {
    return row.timestamp > 1e12 ? row.timestamp : row.timestamp * 1000;
  }
  return null;
}

function snakeUsage(u) {
  if (!u || typeof u !== "object") return null;
  const byModelIn = u.by_model || u.modelUsage || {};
  const byModel = {};
  for (const [id, m] of Object.entries(byModelIn)) {
    const one = snakeUsage(m);
    if (one) {
      delete one.by_model;
      byModel[id] = one;
    }
  }
  return {
    input_tokens: Number(u.input_tokens ?? u.inputTokens) || 0,
    output_tokens: Number(u.output_tokens ?? u.outputTokens) || 0,
    cached_read_tokens: Number(u.cached_read_tokens ?? u.cachedReadTokens) || 0,
    cache_creation_tokens: Number(u.cache_creation_tokens ?? u.cacheCreationTokens) || 0,
    reasoning_tokens: Number(u.reasoning_tokens ?? u.reasoningTokens) || 0,
    model_calls: Number(u.model_calls ?? u.modelCalls) || 0,
    api_duration_ms: Number(u.api_duration_ms ?? u.apiDurationMs) || 0,
    usage_is_incomplete: u.usage_is_incomplete === true || u.incomplete === true,
    by_model: byModel,
  };
}

function callUsageFromTurn(u) {
  if (!u || typeof u !== "object") return {};
  return {
    inputTokens: Number(u.inputTokens ?? u.input_tokens) || 0,
    outputTokens: Number(u.outputTokens ?? u.output_tokens) || 0,
    cacheReadTokens: Number(u.cachedReadTokens ?? u.cached_read_tokens) || 0,
    cacheWriteTokens: Number(u.cacheCreationTokens ?? u.cache_creation_tokens) || 0,
    reasoningTokens: Number(u.reasoningTokens ?? u.reasoning_tokens) || 0,
  };
}

function indexUpdates(rows) {
  const completed = [];
  const userChunks = [];
  const tools = new Map();
  for (const row of rows) {
    const update = updateOf(row);
    if (!update) continue;
    const kind = update.sessionUpdate;
    const ts = tsOf(row);
    if (kind === "turn_completed") {
      completed.push({
        promptId: update.prompt_id,
        usage: update.usage || {},
        elapsedMs: Number(update.elapsed_ms) || null,
        stopReason: update.stop_reason,
        ts,
      });
    } else if (kind === "user_message_chunk") {
      const extra = update._meta || {};
      userChunks.push({
        promptIndex: extra.promptIndex,
        modelId: extra.modelId,
        text: update.content && update.content.text,
        ts,
      });
    } else if (kind === "tool_call") {
      const id = update.toolCallId;
      if (!id) continue;
      const prev = tools.get(id) || {};
      tools.set(id, {
        ...prev,
        callId: id,
        name: (update._meta && update._meta["x.ai/tool"] && update._meta["x.ai/tool"].name) || update.title || prev.name,
        arguments: update.rawInput != null ? update.rawInput : prev.arguments,
        startedAt: ts,
        status: "running",
      });
    } else if (kind === "tool_call_update") {
      const id = update.toolCallId;
      if (!id) continue;
      const prev = tools.get(id) || { callId: id };
      if (update.rawInput != null && prev.arguments == null) prev.arguments = update.rawInput;
      if (update.rawOutput != null) prev.result = update.rawOutput;
      if (update.title) prev.title = update.title;
      const status = update.status || (update._meta && update._meta.updateParams && update._meta.updateParams.status);
      if (status === "completed" || status === "Completed") {
        prev.status = "complete";
        prev.endedAt = ts;
      } else if (status === "failed" || status === "error") {
        prev.status = "error";
        prev.endedAt = ts;
      }
      if (prev.startedAt != null && prev.endedAt != null) {
        prev.durationMs = Math.max(0, prev.endedAt - prev.startedAt);
      }
      tools.set(id, prev);
    }
  }
  return { completed, userChunks, tools };
}

/**
 * @param {{ chatHistory?: string, updates?: string, usageJson?: string, summaryJson?: string, promptContextJson?: string, statsJson?: string, sessionId?: string }} files
 */
export function reconstructSession(files) {
  const history = parseJsonl(files.chatHistory || "");
  const updates = parseJsonl(files.updates || "");
  const indexed = indexUpdates(updates);

  let summary = {};
  let promptContext = {};
  let usageFile = null;
  let stats = null;
  try { if (files.summaryJson) summary = JSON.parse(files.summaryJson); } catch { /* ignore */ }
  try { if (files.promptContextJson) promptContext = JSON.parse(files.promptContextJson); } catch { /* ignore */ }
  try { if (files.usageJson) usageFile = JSON.parse(files.usageJson); } catch { /* ignore */ }
  try { if (files.statsJson) stats = JSON.parse(files.statsJson); } catch { /* ignore */ }

  let system = "";
  const messages = [];
  const turns = [];
  let currentTurn = null;
  let pendingReasoning = "";
  const observedTools = new Set();

  function startTurn(index, prompt, startedAt) {
    currentTurn = {
      turn: index,
      prompt,
      promptPreview: previewText(prompt) || ("Turn " + index),
      startedAt: startedAt || null,
      durationMs: null,
      status: "complete",
      calls: [],
      toolCount: 0,
    };
    turns.push(currentTurn);
    pendingReasoning = "";
  }

  for (const rec of history) {
    if (!rec || typeof rec !== "object") continue;
    const type = rec.type;
    if (type === "system") {
      if (!system) system = asString(rec.content);
      continue;
    }
    if (type === "user") {
      const text = textOfUser(rec);
      const query = extractUserQuery(text);
      const isTurn = Number.isInteger(rec.prompt_index) || (Boolean(query) && rec.synthetic_reason == null);
      if (isTurn) {
        const index = Number.isInteger(rec.prompt_index) ? rec.prompt_index + 1 : turns.length + 1;
        startTurn(index, query || text, null);
      }
      messages.push({
        role: "user",
        content: rec.content,
        synthetic_reason: rec.synthetic_reason || undefined,
        prompt_index: rec.prompt_index,
      });
      continue;
    }
    if (type === "reasoning") {
      const bit = reasoningText(rec);
      if (bit) pendingReasoning = pendingReasoning ? pendingReasoning + "\n" + bit : bit;
      continue;
    }
    if (type === "assistant") {
      if (!currentTurn) startTurn(turns.length + 1, "", null);
      const toolCalls = Array.isArray(rec.tool_calls) ? rec.tool_calls : [];
      const parsedCalls = toolCalls.map((tc) => {
        const name = asString(tc && tc.name);
        if (name) observedTools.add(name);
        return {
          id: asString(tc && tc.id),
          name,
          arguments: parseArgs(tc && tc.arguments),
        };
      });
      const snapshot = messages.slice();
      const tools = parsedCalls.map((tc) => {
        const fromUpdate = indexed.tools.get(tc.id) || {};
        return {
          callId: tc.id,
          name: tc.name || fromUpdate.name || "",
          arguments: tc.arguments != null ? tc.arguments : fromUpdate.arguments,
          result: fromUpdate.result != null ? fromUpdate.result : null,
          error: fromUpdate.error || null,
          status: fromUpdate.status || "running",
          durationMs: fromUpdate.durationMs != null ? fromUpdate.durationMs : null,
        };
      });
      currentTurn.calls.push({
        id: currentTurn.turn + ":" + (currentTurn.calls.length + 1),
        turn: currentTurn.turn,
        number: currentTurn.calls.length + 1,
        startedAt: null,
        durationMs: null,
        status: "complete",
        model: rec.model_id || rec.model_fingerprint || rec.model || "",
        system,
        messages: snapshot,
        toolDefs: [],
        response: {
          reasoning: pendingReasoning,
          content: typeof rec.content === "string" ? rec.content : "",
          tool_calls: parsedCalls,
        },
        usage: {},
        tools,
      });
      currentTurn.toolCount += tools.length;
      pendingReasoning = "";
      messages.push({
        role: "assistant",
        content: rec.content,
        tool_calls: parsedCalls,
        model_id: rec.model_id,
      });
      continue;
    }
    if (type === "tool_result") {
      const id = rec.tool_call_id;
      const result = rec.content;
      outer: for (let t = turns.length - 1; t >= 0; t--) {
        const calls = turns[t].calls;
        for (let c = calls.length - 1; c >= 0; c--) {
          const tool = calls[c].tools.find((x) => x.callId === id);
          if (tool) {
            if (tool.result == null) tool.result = result;
            if (tool.status === "running") tool.status = "complete";
            break outer;
          }
        }
      }
      messages.push({ role: "tool", tool_call_id: id, content: result });
    }
  }

  turns.forEach((turn, i) => {
    const done = indexed.completed[i];
    const chunk = indexed.userChunks.find((c) => c.promptIndex === turn.turn - 1) || indexed.userChunks[i];
    if (chunk && chunk.ts != null) turn.startedAt = chunk.ts;
    if (chunk && chunk.modelId && turn.calls[0] && !turn.calls[0].model) {
      for (const call of turn.calls) {
        if (!call.model) call.model = chunk.modelId;
      }
    }
    if (done) {
      turn.durationMs = done.elapsedMs;
      if (turn.startedAt == null && done.ts != null && done.elapsedMs != null) {
        turn.startedAt = done.ts - done.elapsedMs;
      }
      const usage = callUsageFromTurn(done.usage);
      const last = turn.calls[turn.calls.length - 1];
      if (last) last.usage = usage;
      const models = done.usage && done.usage.modelUsage ? Object.keys(done.usage.modelUsage) : [];
      if (models.length) {
        for (const call of turn.calls) {
          if (!call.model) call.model = models[0];
        }
      }
    }
    if (turn.calls.some((c) => c.status === "error" || c.tools.some((t) => t.status === "error"))) {
      turn.status = "error";
    }
  });

  const observed = [...observedTools];
  for (const turn of turns) {
    for (const call of turn.calls) {
      call.toolDefs = observed.length
        ? [{ note: "grok jsonl 沒有寫入 API tools[] schema", observed_tool_names: observed }]
        : [{ note: "grok jsonl 沒有寫入 API tools[] schema" }];
    }
  }

  const requestCount = turns.reduce((n, t) => n + t.calls.length, 0);
  const toolCount = turns.reduce((n, t) => n + t.toolCount, 0);
  const durationMs = turns.reduce((n, t) => n + (Number(t.durationMs) || 0), 0);
  const sessionId = files.sessionId
    || (summary.info && summary.info.id)
    || (stats && stats.session_id)
    || "";

  if (!stats || stats.format !== "grok-session-stats/v1") {
    const usage = snakeUsage((usageFile && usageFile.totals) || {}) || {
      input_tokens: 0,
      output_tokens: 0,
      cached_read_tokens: 0,
      cache_creation_tokens: 0,
      reasoning_tokens: 0,
      model_calls: requestCount,
      api_duration_ms: 0,
      usage_is_incomplete: false,
      by_model: {},
    };
    if (usageFile && usageFile.by_model) {
      usage.by_model = {};
      for (const [id, m] of Object.entries(usageFile.by_model)) {
        const one = snakeUsage(m);
        if (one) {
          delete one.by_model;
          usage.by_model[id] = one;
        }
      }
    }
    const toolMap = new Map();
    for (const turn of turns) {
      for (const call of turn.calls) {
        for (const tool of call.tools) {
          const name = tool.name || "(unknown)";
          const prev = toolMap.get(name) || { name, count: 0, duration_ms: 0 };
          prev.count += 1;
          prev.duration_ms += Number(tool.durationMs) || 0;
          toolMap.set(name, prev);
        }
      }
    }
    stats = {
      format: "grok-session-stats/v1",
      exported_at: summary.updated_at || null,
      session_id: sessionId,
      usage,
      isolation: {
        memory_enabled: promptContext.memory_enabled === true,
        memory_context_injected: false,
        memory_tool_calls: 0,
      },
      agents: [
        {
          session_id: sessionId,
          kind: "parent",
          subagent_type: null,
          usage,
          tool_call_count: toolCount,
          tools: [...toolMap.values()],
        },
      ],
      source: "session-jsonl",
    };
  }

  return {
    stats,
    workflow: {
      turns,
      requestCount,
      toolCount,
      durationMs,
    },
    meta: {
      session_id: sessionId,
      title: summary.generated_title || summary.session_summary || sessionId,
      cwd: (summary.info && summary.info.cwd) || promptContext.working_directory || "",
      files: ["chat_history.jsonl", "updates.jsonl"].filter((name) => {
        if (name === "chat_history.jsonl") return Boolean(files.chatHistory);
        return Boolean(files.updates);
      }),
    },
  };
}
