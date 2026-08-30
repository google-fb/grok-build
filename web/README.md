# Session inspector（web）

獨立的靜態頁 + 本機小伺服器。讀 grok session 目錄裡的 `chat_history.jsonl` / `updates.jsonl`（以及可選的 `usage.json`），還原成 DSH「工作流 JSON」那種 **REQUEST / RESPONSE / tool** 卡片，點開可看 system prompt、messages、tool 參數與結果。

這不是官方 grok 內建 UI，也不是 DSH plugin。它只吃 **本分支編出來的 grok** 寫下的 session 檔。

## 為什麼要開伺服器

瀏覽器不能自己掃磁碟。`node serve.mjs` 會讀：

- `GROK_HOME`
- `~/.grok`
- `homes.json` 裡的額外 home（路徑陣列）
- 命令列再傳的路徑：`node serve.mjs /path/to/grok-home`

然後提供：

- `GET /api/sessions`
- `GET /api/session/<id>` → 從該 session 的 jsonl 還原 turns / tool JSON

## 怎麼開

需要 Node.js 22+。在 **這個 `web/` 目錄**：

```sh
cd web
node serve.mjs
```

瀏覽器開 http://127.0.0.1:4177

可選：把實驗用的 `GROK_HOME` 傳進去，例如 Linux 測試機：

```sh
node serve.mjs "$GROK_HOME"
```

Windows PowerShell：

```powershell
node serve.mjs $env:GROK_HOME
```

頁面載入後會列出 sessions。點 **REQUEST** 看 system / messages；點工具卡片看參數與結果 JSON。

也可以：

- 拖整個 session 資料夾（裡面要有 `chat_history.jsonl`）
- 拖 `/export-json` 產出的 `grok-session-stats/v1` JSON（伺服器開著時會再用 `session_id` 去對 jsonl）
- 按「載入 sample.json」看彙總表 fixture

不要用 `file://` 開 `index.html`：ES module 常被擋。

## 和 DSH 套件的差別

Grok 的 jsonl **有**：

- system prompt
- 每輪 messages
- tool 參數 / 結果
- 每輪 token（`turn_completed.usage`）

**沒有** 當次 API `tools[]` schema（DSH 的 `request/header` 才會記）。工具定義欄會標「未記錄」。每輪 token 是 grok 的 turn 合計，不是每一步分開計。

## 不要提交的東西

`homes.json` 在 repo 裡是空陣列。若你在本機填了路徑，請不要把含個資的 home、也 **不要把 `auth.json`** 推進 Git。
