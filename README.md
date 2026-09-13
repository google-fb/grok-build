<div align="center">

<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://media.x.ai/v1/website/spacexai-symbol-white-transparent-0c31957f.png">
    <source media="(prefers-color-scheme: light)" srcset="https://media.x.ai/v1/website/spacexai-symbol-black-transparent-6435cf42.png">
    <img alt="SpaceXAI logo" src="https://media.x.ai/v1/website/spacexai-symbol-black-transparent-6435cf42.png" width="96">
  </picture>
  <br>
  Grok Build (<code>grok</code>)
</h1>

**Grok Build** is SpaceXAI's terminal-based AI coding agent. It runs as a
full-screen TUI that understands your codebase, edits files, executes shell
commands, searches the web, and manages long-running tasks — interactively,
headlessly for scripting/CI, or embedded in editors via the Agent Client
Protocol (ACP).

[Installing the released binary](#installing-the-released-binary) ·
[Building from source](#building-from-source) ·
[Documentation](#documentation) ·
[Repository layout](#repository-layout) ·
[Development](#development) ·
[Contributing](#contributing) ·
[License](#license)

![Grok Build TUI](https://media.x.ai/v1/website/universe-tui-screenshot-6f7a0837.png)

**Learn more about Grok Build at [x.ai/cli](https://x.ai/cli)**

This repository contains the Rust source for the `grok` CLI/TUI and its agent
runtime. It is synced periodically from the SpaceXAI monorepo.

A small `SOURCE_REV` file at the root records the full monorepo commit SHA
for the version of the code present in this tree.

</div>

---

## 本分支（`tuco-web-testing`）改了什麼

這條 branch 不是官方 grok 發行檔。終端機裡的 `grok`（例如 `~/.grok/bin/grok`）**沒有** 下列功能。要測，請用本 repo 編出來的 `target/debug/xai-grok-pager`（Windows 為 `.exe`）。

相對 upstream `SOURCE_REV` `d5a0335a…` / 本樹 `bc7f02ed`，多了：

1. **Usage 落盤**  
   每次計費後把 `UsageLedger` 寫進 session 目錄的 `usage.json`。TUI 重開或 `/resume` 會還原 token，不再只活在 RAM。

2. **`/export-json`（pager builtin，不會進模型）**  
   TUI：`/export-json [filename]`  
   CLI：`xai-grok-pager export-json <session-id> [path]`  
   產出 `grok-session-stats/v1`：根層 token、各 agent、工具次數、記憶隔離旗標（`memory_enabled` / `memory_context_injected` / `memory_tool_calls`）。

3. **Session inspector（獨立資料夾 [`web/`](web/README.md)）**  
   `node serve.mjs` 自動讀 `chat_history.jsonl` / `updates.jsonl`，還原 REQUEST / RESPONSE / tool JSON，可展開查看呼叫參數與結果。

4. **Windows 編譯**  
   - `protoc --dependency_out=/dev/stdout` 在 Windows 會失敗，改成跳過這段 rerun 掃描。  
   - MSVC debug 預設 1MB stack 會爆；`.cargo/config.toml` 加上 `/STACK:16777216`。Linux / macOS 不受影響。

實驗時請關記憶：`GROK_MEMORY=0` 與 `--no-memory`。不要打 `/memory on`、`/remember`、`/flush`、`/dream`。

### 此修正分支：推論 API 409 重試

推論 API 回傳 HTTP 409 時，沿用 sampler 的有限重試、退避等待與 jitter，
在同一次模型請求內恢復。`GROK_MAX_RETRIES`／model 的 `max_retries` 控制既有上限；
伺服器的 `x-should-retry: false`、context overflow、取消與重試耗盡仍會停止。
這項修改在推論層重送請求，不會由 host 重啟對話或直接重送網站操作；
未知的持續性 409 仍可能無法恢復。

要採用必須從本分支重新編譯，已安裝的 `grok` 不會自動改變。
正式比較應讓兩個方法使用相同凍結版本與重試設定，等待也計入既有時間上限。
本修正的假 HTTP 測試不需要模型帳號、費用或網站測試資料。

### 用這包，不要用已安裝的 grok

```sh
# Linux / macOS
export GROK_HOME="$PWD/.linux-test-home"
export GROK_MEMORY=0
mkdir -p "$GROK_HOME"
# 可把本機 ~/.grok/auth.json 拷進 $GROK_HOME，或第一次開 TUI 再登入
cargo build -p xai-grok-pager-bin
./target/debug/xai-grok-pager --no-memory
```

```powershell
# Windows：不要打 grok
$pager = "$PWD\target\debug\xai-grok-pager.exe"
$env:GROK_HOME = "$PWD\.my-test-home"
$env:GROK_MEMORY = "0"
New-Item -ItemType Directory -Force $env:GROK_HOME | Out-Null
Copy-Item "$env:USERPROFILE\.grok\auth.json" "$env:GROK_HOME\auth.json" -Force
& $pager --no-memory
```

跑完後：

```sh
./target/debug/xai-grok-pager export-json <session-id> out.json
cd web && node serve.mjs "$GROK_HOME"
# 瀏覽器 http://127.0.0.1:4177
```

Windows 的 `.exe` 不能拿到 Linux 跑；測試機請用同一份原始碼在 Linux 本機 `cargo build -p xai-grok-pager-bin`（不要拷 `target/`）。`web/` 不用編 Rust。

不要 commit `auth.json`、`.dev-flow-home/`、或任何 `GROK_HOME`。

---

## Installing the released binary

Prebuilt binaries are published for macOS, Linux, and Windows:

```sh
curl -fsSL https://x.ai/cli/install.sh | bash   # macOS / Linux / Git Bash
irm https://x.ai/cli/install.ps1 | iex          # Windows PowerShell
grok --version
```

See the [changelog](https://x.ai/build/changelog) for the latest fixes,
features, and improvements in each release.

## Building from source

Requirements:

- **Rust** — the toolchain is pinned by [`rust-toolchain.toml`](rust-toolchain.toml);
  `rustup` installs it automatically on first build.
- **[DotSlash](https://dotslash-cli.com)** — required so hermetic tools under
  [`bin/`](bin/) (notably [`bin/protoc`](bin/protoc)) can download and run.
  Install it and ensure `dotslash` is on your `PATH` **before** building:

  ```sh
  cargo install dotslash
  # or: prebuilt packages — https://dotslash-cli.com/docs/installation/
  /usr/bin/env dotslash --help   # sanity check
  ```

- **protoc** — proto codegen resolves [`bin/protoc`](bin/protoc) via DotSlash,
  or falls back to a `protoc` on `PATH` / `$PROTOC`.
- macOS and Linux are supported build hosts; Windows builds are best-effort
  and not currently tested from this tree.

```sh
cargo run -p xai-grok-pager-bin              # build + launch the TUI
cargo build -p xai-grok-pager-bin --release  # release binary: target/release/xai-grok-pager
cargo check -p xai-grok-pager-bin            # fast validation
```

The binary artifact is named `xai-grok-pager`; official installs ship it as
`grok`. On first launch it opens your browser to authenticate — see the
[authentication guide](crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md).

## Documentation

Full online documentation is available at
[docs.x.ai/build/overview](https://docs.x.ai/build/overview).

The user guide ships with the pager crate:
[`crates/codegen/xai-grok-pager/docs/user-guide/`](crates/codegen/xai-grok-pager/docs/user-guide/)
— getting started, keyboard shortcuts, slash commands, configuration, theming,
MCP servers, skills, plugins, hooks, headless mode, sandboxing, and more.

## Repository layout

| Path | Contents |
|------|----------|
| `crates/codegen/xai-grok-pager-bin` | Composition-root package; builds the `xai-grok-pager` binary |
| `crates/codegen/xai-grok-pager` | The TUI: scrollback, prompt, modals, rendering |
| `crates/codegen/xai-grok-shell` | Agent runtime + leader/stdio/headless entry points |
| `crates/codegen/xai-grok-tools` | Tool implementations (terminal, file edit, search, ...) |
| `crates/codegen/xai-grok-workspace` | Host filesystem, VCS, execution, checkpoints |
| `crates/codegen/...` | The rest of the CLI crate closure (config, MCP, markdown, sandbox, ...) |
| `crates/common/`, `crates/build/`, `prod/mc/` | Small shared leaf crates pulled in by the closure |
| `third_party/` | Vendored upstream source (Mermaid diagram stack) — see below |
| `web/` | **This branch:** session inspector (jsonl → REQUEST/RESPONSE/tool JSON). See [`web/README.md`](web/README.md). |

> [!IMPORTANT]
> The root `Cargo.toml` (workspace members, dependency versions, lints,
> profiles) is **generated** — treat it as read-only. Prefer editing per-crate
> `Cargo.toml` files.

## Development

```sh
cargo check -p <crate>        # always target specific crates; full-workspace builds are slow
cargo test -p xai-grok-config # per-crate tests
cargo clippy -p <crate>       # lint config: clippy.toml at the repo root
cargo fmt --all               # rustfmt.toml at the repo root
```

## Contributing

> [!NOTE]
> External contributions are not accepted. See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## License

First-party code in this repository is licensed under the **Apache License,
Version 2.0** — see [`LICENSE`](LICENSE).

Third-party and vendored code remains under its original licenses. See:

- [`THIRD-PARTY-NOTICES`](THIRD-PARTY-NOTICES) — crates.io / git dependencies,
  bundled UI themes, and **in-tree source ports** (including openai/codex and
  sst/opencode tool implementations)
- [`crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md`](crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md)
  — crate-local notice for the codex and opencode ports (license texts +
  Apache §4(b) change notice)
- [`third_party/NOTICE`](third_party/NOTICE) — vendored Mermaid-stack index
