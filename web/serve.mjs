import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { reconstructSession } from "./reconstruct.mjs";

const DIR = path.dirname(fileURLToPath(import.meta.url));
const PORT = Number(process.env.PORT) || 4177;

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

function existsDir(p) {
  try {
    return fs.statSync(p).isDirectory();
  } catch {
    return false;
  }
}

function addHome(list, raw) {
  if (!raw) return;
  const abs = path.resolve(String(raw));
  if (!existsDir(path.join(abs, "sessions"))) return;
  if (!list.includes(abs)) list.push(abs);
}

export function grokHomes() {
  const list = [];
  addHome(list, process.env.GROK_HOME);
  addHome(list, path.join(os.homedir(), ".grok"));
  try {
    const extra = JSON.parse(fs.readFileSync(path.join(DIR, "homes.json"), "utf8"));
    if (Array.isArray(extra)) extra.forEach((p) => addHome(list, p));
  } catch {
    // homes.json is optional
  }
  for (const arg of process.argv.slice(2)) {
    if (!arg.startsWith("-")) addHome(list, arg);
  }
  return list;
}

function readOptional(filePath) {
  try {
    return fs.readFileSync(filePath, "utf8");
  } catch {
    return "";
  }
}

export function listSessions(homes = grokHomes()) {
  const out = [];
  for (const home of homes) {
    const root = path.join(home, "sessions");
    if (!existsDir(root)) continue;
    let cwdNames = [];
    try {
      cwdNames = fs.readdirSync(root, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const cwdEnc of cwdNames) {
      if (!cwdEnc.isDirectory()) continue;
      const cwdDir = path.join(root, cwdEnc.name);
      let ids = [];
      try {
        ids = fs.readdirSync(cwdDir, { withFileTypes: true });
      } catch {
        continue;
      }
      for (const sid of ids) {
        if (!sid.isDirectory()) continue;
        const dir = path.join(cwdDir, sid.name);
        if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(sid.name)) continue;
        const historyPath = path.join(dir, "chat_history.jsonl");
        const updatesPath = path.join(dir, "updates.jsonl");
        if (!fs.existsSync(historyPath) && !fs.existsSync(updatesPath)) continue;
        let summary = {};
        try {
          summary = JSON.parse(readOptional(path.join(dir, "summary.json")) || "{}");
        } catch {
          summary = {};
        }
        const cwd = (summary.info && summary.info.cwd) || cwdEnc.name;
        if (/[\\/](?:Temp|\.tmp|tmp)[\\/]/i.test(String(cwd)) || /\\Temp\\/i.test(dir)) continue;
        out.push({
          session_id: sid.name,
          home,
          cwd,
          title: summary.generated_title || summary.session_summary || sid.name,
          updated_at: summary.updated_at || summary.last_active_at || null,
          dir,
        });
      }
    }
  }
  out.sort((a, b) => String(b.updated_at || "").localeCompare(String(a.updated_at || "")));
  return out;
}

export function loadSessionDir(dir, sessionId) {
  const files = {
    chatHistory: readOptional(path.join(dir, "chat_history.jsonl")),
    updates: readOptional(path.join(dir, "updates.jsonl")),
    usageJson: readOptional(path.join(dir, "usage.json")),
    summaryJson: readOptional(path.join(dir, "summary.json")),
    promptContextJson: readOptional(path.join(dir, "prompt_context.json")),
    statsJson: "",
    sessionId,
  };
  return reconstructSession(files);
}

function send(res, status, body, headers = {}) {
  const payload = typeof body === "string" || Buffer.isBuffer(body) ? body : JSON.stringify(body);
  res.writeHead(status, { "cache-control": "no-store", ...headers });
  res.end(payload);
}

function sendJson(res, status, body) {
  send(res, status, JSON.stringify(body), { "content-type": "application/json; charset=utf-8" });
}

function serveStatic(urlPath, res) {
  const rel = urlPath === "/" ? "index.html" : decodeURIComponent(urlPath.replace(/^\//, ""));
  if (rel.includes("..") || path.isAbsolute(rel)) {
    send(res, 400, "bad path");
    return;
  }
  const filePath = path.join(DIR, rel);
  if (!filePath.startsWith(DIR)) {
    send(res, 400, "bad path");
    return;
  }
  fs.readFile(filePath, (err, data) => {
    if (err) {
      send(res, 404, "not found");
      return;
    }
    const ext = path.extname(filePath).toLowerCase();
    send(res, 200, data, { "content-type": MIME[ext] || "application/octet-stream" });
  });
}

function findSession(id) {
  const matches = listSessions().filter((s) => s.session_id === id);
  return matches[0] || null;
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url || "/", "http://127.0.0.1");
  if (req.method === "GET" && url.pathname === "/api/homes") {
    sendJson(res, 200, { homes: grokHomes() });
    return;
  }
  if (req.method === "GET" && url.pathname === "/api/sessions") {
    sendJson(res, 200, {
      homes: grokHomes(),
      sessions: listSessions().map((s) => ({
        session_id: s.session_id,
        home: s.home,
        cwd: s.cwd,
        title: s.title,
        updated_at: s.updated_at,
      })),
    });
    return;
  }
  const sessionMatch = url.pathname.match(/^\/api\/session\/([^/]+)$/);
  if (req.method === "GET" && sessionMatch) {
    const id = decodeURIComponent(sessionMatch[1]);
    const found = findSession(id);
    if (!found) {
      sendJson(res, 404, { error: "找不到 session " + id + " 的 jsonl。把 GROK_HOME 或 session 資料夾加到 homes.json。" });
      return;
    }
    try {
      const loaded = loadSessionDir(found.dir, found.session_id);
      sendJson(res, 200, { ...loaded, listed: found });
    } catch (err) {
      sendJson(res, 500, { error: err && err.message ? err.message : String(err) });
    }
    return;
  }
  if (req.method === "GET") {
    serveStatic(url.pathname, res);
    return;
  }
  send(res, 405, "method not allowed");
});

server.listen(PORT, "127.0.0.1", () => {
  const homes = grokHomes();
  process.stdout.write("Grok session viewer  http://127.0.0.1:" + PORT + "\n");
  process.stdout.write("homes:\n" + (homes.length ? homes.map((h) => "  " + h).join("\n") : "  (none)") + "\n");
});
