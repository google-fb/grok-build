# Plan: persist usage + `/export-json` + viewer

Drop-in brief for a **new Grok session** whose cwd is this repo
(`…/deepseek/grok-build/grok-build`). Do not add skills, hooks, MCP, or
prompt-template text. Memory stays off (`GROK_MEMORY=0`). This work must
not change what the model sees.

Agreed product:

1. Persist `UsageLedger` so billed tokens survive reboot / TUI restart.
2. Pager builtin `/export-json` (never sent to the model) dumps a JSON
   bundle after `/resume`.
3. Standalone HTML viewer (outside this repo) reads that JSON: input /
   output tokens, tool calls, per-agent rows, memory-isolation flags.

---

## Where to work (do not use the OSCE tree as the patch target)

| Tree | `SOURCE_REV` | Role |
|---|---|---|
| **This repo** (patch here) | `d5a0335a47221e8c9519936cb693e9b6450227ec` | Experiment TUI. Newer crates (`xai-grok-status-line`, `xai-grok-session-events`). No OSCE skills. `target/` is empty → first `cargo` will be slow. |
| `C:\Users\dpes8\OneDrive\文件\Github3\grok-build-osce` | `0f4d7c91b8b2b408333f6de1e8a76cb8eaa71899` | OSCE / OpenRouter fork. Has `target/debug` + `target/release`. **Do not patch it. Do not copy `target/` here.** Different crate graph; fingerprint mismatch. Skills would contaminate the experiment. |

### Reuse from OSCE (patterns only, copy ideas not crates)

- CLI export shape: `crates/codegen/xai-grok-pager/src/export_cmd.rs`
  (`export <session-id> [output]`). Add a sibling `export-json`.
- Session-dir walk: `scripts/osce-exam-save.py` `find_session_dir`
  (`~/.grok/sessions/<encoded-cwd>/<session-id>/`). Honor `GROK_PAGER_BIN`.
  Never auto-run cargo from a helper script.
- Atomic write: `storage/mod.rs` `write_bytes_atomic` (same as `signals.json`).
- Resume lesson (`project-plan/HANDOFF_V2.md` §五): every per-model /
  per-session persist path must be tested on **new session and `/resume`**.
- `UsageLedger` in OSCE is still RAM-only (`not serialized`). Nothing to
  lift for durability.

Viewer UI may borrow layout ideas from
`C:\Users\dpes8\OneDrive\文件\dsh-plugin-workflow-json` (turn list +
metrics). **Do not parse DSH events.** Parser is grok ACP / our JSON
schema only.

---

## Build rules (avoid a full rebuild)

Windows first debug of `xai-grok-pager-bin` on a cold `target/` is
expensive (tens of minutes). After that, incremental is minutes.

```bat
set CARGO_INCREMENTAL=1
cargo check -p xai-chat-state --offline
cargo test  -p xai-chat-state --lib -- usage --offline
cargo test  -p xai-grok-shell --lib -- usage persist resume --offline
cargo test  -p xai-grok-pager --lib -- export_json --offline
cargo build -p xai-grok-pager-bin
```

Forbidden:

- `cargo build --release` (unless the user later asks).
- `cargo test` with no `-p` / no filter (OSCE `target/` once hit 34 GB).
- `pty_e2e`, full pager suite.
- Copying OSCE `target/` into this tree.
- `CARGO_INCREMENTAL=0` (that was an OSCE disk workaround; here we want
  incremental).

Binary: `target/debug/xai-grok-pager.exe`. Launch that file for
acceptance, not a store-installed `grok`.

`--offline` after the first successful fetch. If `cargo check` fails
because this tree was never built, drop `--offline` for that one run.

---

## Phase A — persist `UsageLedger` (required for reboot)

File: `{session_dir}/usage.json` next to `signals.json`.

Write (atomic + fsync, same helper as signals) after every:

- `record_main_loop_call`
- `record_subagent`
- `mark_usage_incomplete` (session bit)

Not only at turn end. A mid-turn power loss must keep already-billed
calls.

Read on spawn / resume (same site as `persisted_signals` in
`acp_session_impl/spawn.rs`) and restore into `session_usage`. Missing
file → empty ledger (old sessions).

Serialize `UsageLedger` / `UsageTotals` (today they are not
`Serialize`). Keep field names stable. Child sessions get their own
file; parent fold stays as today. Viewer must not add child totals on
top of parent.

Likely touch:

- `xai-chat-state/src/usage.rs` — serde
- `xai-chat-state` actor mutations — emit persist after record
- `xai-grok-shell` persistence actor — `PersistenceMsg::Usage`
- `storage/jsonl` — `USAGE_FILE`, `write_usage` / `read_usage`
- spawn restore path

Do not put billed tokens only in `signals.json`. Signals already restore
tool counts / TTFT averages, not billed input.

### Phase A acceptance

- Unit: record two main-loop calls + one subagent fold → write
  `usage.json` → new actor restore → same `input_tokens`,
  `output_tokens`, `cached_read_tokens`, `model_calls`,
  `main_loop_model_calls`, `incomplete`.
- Crash-style: kill after the persist of call 1, before call 2 → restore
  has call 1 only.
- Resume: `/usage` after process restart shows pre-restart billed
  totals, not zeros.
- Old session dir without `usage.json` still loads.

---

## Phase B — pager `/export-json` (no skill)

Copy `/export` (`slash/commands/export.rs`). New name `export-json`.

Must add `"export-json"` to `PAGER_COMMAND_KEYS` in
`xai-grok-shell/src/session/slash_commands.rs`. Pager builtins are never
sent to the model; missing this key would send `/export-json` as chat
text and contaminate the experiment.

Slash: `/export-json [filename]`  
CLI: `xai-grok-pager export-json <session-id> [path]` (mirror
`export_cmd.rs`).

After `/resume`, the command reads the **restored** ledger, not an empty
live one.

Walk parent `updates.jsonl` + `subagents/*/meta.json` → child session
dirs. Count tool calls from ACP tool_call updates. Emit one JSON file.

Schema `grok-session-stats/v1`:

```json
{
  "format": "grok-session-stats/v1",
  "exported_at": "RFC3339",
  "session_id": "",
  "usage": {
    "input_tokens": 0,
    "output_tokens": 0,
    "cached_read_tokens": 0,
    "cache_creation_tokens": 0,
    "reasoning_tokens": 0,
    "model_calls": 0,
    "api_duration_ms": 0,
    "usage_is_incomplete": false,
    "by_model": {}
  },
  "isolation": {
    "memory_enabled": false,
    "memory_context_injected": false,
    "memory_tool_calls": 0
  },
  "agents": [
    {
      "session_id": "",
      "kind": "parent",
      "subagent_type": null,
      "usage": {},
      "tool_call_count": 0,
      "tools": [{ "name": "read_file", "count": 1, "duration_ms": 0 }]
    }
  ]
}
```

`usage` at the root = parent ledger (already folded). `agents[].usage` =
that agent's own file. Isolation flags: scan `chat_history.jsonl` for
`<memory-context>` and tool names `memory_search` / `memory_get`.

Leave Markdown `/export` unchanged.

### Phase B acceptance

- `PAGER_COMMAND_KEYS` contains `export-json`; pager builtin test still
  passes.
- Dispatching `/export-json` does not enqueue a user prompt (same as
  `/export`).
- After a session with ≥1 model call and ≥1 tool: JSON has non-zero
  tokens and matching tool counts.
- Restart TUI → `/resume` → `/export-json` → tokens match pre-restart
  `/usage` (not zero).
- Two agents: parent `usage` is the fold; `agents` has two rows; summing
  child+parent input ≠ parent (no double count in the UI).
- Filename `~` expansion + parent-dir create, same as `/export`.

---

## Phase C — standalone viewer (no cargo)

New folder **outside** both grok trees, e.g.
`C:\Users\dpes8\OneDrive\文件\grok-session-stats-viewer`.

Static `index.html` + JS. Drag-drop / file input of the JSON. No
server, no DSH plugin, no grok skill.

Show: session id, root tokens, cache hit if den ≠ 0, tool table,
per-agent table, isolation three flags (fail red if any memory used).

Can be written in parallel with A/B using a fixture JSON. Does not
block on cargo.

### Phase C acceptance

- Fixture with two agents, incomplete flag, `memory_tool_calls: 0`
  renders totals from root `usage`, lists both agents, shows isolation
  OK.
- Fixture with `memory_context_injected: true` or `memory_tool_calls > 0`
  shows a hard fail banner.
- Opening a DSH workflow JSON shows a parse error, not a fake chart.

---

## Phase D — experiment loop (human, after A–C)

```bat
set GROK_MEMORY=0
target\debug\xai-grok-pager.exe --no-memory
```

New session (not resume) → run work → reboot or kill TUI → `/resume` →
`/export-json run.json` → open in the viewer.

Two repeats: two session ids; both isolation flags clean; workspace
reset between runs (`git checkout` / clean worktree). `/remember`
`/flush` `/dream` `/memory on` forbidden.

---

## Suggested split for new sessions

1. **This cwd, this plan, Phases A+B.** Rust only. Stop when
   `target/debug/xai-grok-pager.exe` exists and `/export-json` after
   resume has tokens.
2. **Viewer cwd, Phase C.** No rust. Can start immediately with a
   fixture.
3. **Do not** open a session in `grok-build-osce` for this feature.

First cargo in this tree is the long wait. After it succeeds, keep the
`target/` and iterate. If disk is tight, `cargo clean -p xai-grok-pager`
is enough; do not wipe all of `target/`.
