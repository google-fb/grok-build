//! CLI `export-json` and the JSON bundle builder used by `/export-json`.
//!
//! Reads restored session files (`usage.json`, `updates.jsonl`,
//! `chat_history.jsonl`, `subagents/*/meta.json`). Does not talk to a live
//! actor.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const FORMAT: &str = "grok-session-stats/v1";
const MEMORY_CONTEXT_TAG: &str = "<memory-context>";
const MEMORY_SEARCH: &str = "memory_search";
const MEMORY_GET: &str = "memory_get";

#[derive(Debug, clap::Args, Clone)]
pub struct ExportJsonArgs {
    /// Session ID to export
    pub session_id: String,
    /// Output file path (default: stdout)
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionStatsExport {
    pub format: String,
    pub exported_at: String,
    pub session_id: String,
    pub usage: ExportUsage,
    pub isolation: IsolationFlags,
    pub agents: Vec<AgentStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ExportUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
    pub model_calls: u64,
    pub api_duration_ms: u64,
    pub usage_is_incomplete: bool,
    pub by_model: BTreeMap<String, ExportUsageTotals>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ExportUsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_read_tokens: u64,
    pub cache_creation_tokens: u64,
    pub reasoning_tokens: u64,
    pub model_calls: u64,
    pub api_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct IsolationFlags {
    pub memory_enabled: bool,
    pub memory_context_injected: bool,
    pub memory_tool_calls: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentStats {
    pub session_id: String,
    pub kind: String,
    pub subagent_type: Option<String>,
    pub usage: ExportUsage,
    pub tool_call_count: u64,
    pub tools: Vec<ToolStat>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolStat {
    pub name: String,
    pub count: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct OnDiskUsage {
    totals: OnDiskTotals,
    by_model: BTreeMap<String, OnDiskTotals>,
    incomplete: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct OnDiskTotals {
    input_tokens: u64,
    output_tokens: u64,
    cached_read_tokens: u64,
    cache_creation_tokens: u64,
    reasoning_tokens: u64,
    model_calls: u64,
    api_duration_ms: u64,
}

#[derive(Debug, Deserialize)]
struct SubagentMeta {
    #[serde(default)]
    child_session_id: Option<String>,
    #[serde(default)]
    subagent_type: Option<String>,
}

pub fn run(args: ExportJsonArgs) -> Result<()> {
    tracing::info!(session_id = %args.session_id, "export_json: starting session stats export");
    let bundle = export_session_stats(&args.session_id)?;
    let json = serde_json::to_string_pretty(&bundle)?;

    if let Some(path) = args.output {
        let expanded = expand_output_path(&path);
        write_json_file(&expanded, &json)?;
        tracing::info!(
            session_id = %args.session_id,
            path = %expanded.display(),
            bytes = json.len(),
            "export_json: wrote stats to file"
        );
        eprintln!("Session stats exported to {}", expanded.display());
    } else {
        std::io::stdout().write_all(json.as_bytes())?;
        std::io::stdout().write_all(b"\n")?;
    }
    Ok(())
}

pub fn export_session_stats(session_id: &str) -> Result<SessionStatsExport> {
    let session_dir = xai_grok_shell::session::persistence::find_session_dir_by_id(session_id)
        .with_context(|| format!("Session '{session_id}' not found."))?;
    Ok(build_from_session_dir(&session_dir, session_id))
}

pub fn expand_output_path(path: &Path) -> PathBuf {
    PathBuf::from(shellexpand::tilde(&path.to_string_lossy()).as_ref())
}

pub fn write_json_file(path: &Path, json: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    std::fs::write(path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

pub fn build_from_session_dir(session_dir: &Path, session_id: &str) -> SessionStatsExport {
    let parent_usage = read_usage(&session_dir.join("usage.json"));
    let parent_tools = count_tools(&session_dir.join("updates.jsonl"));
    let isolation = scan_isolation(&session_dir.join("chat_history.jsonl"));

    let mut agents = vec![AgentStats {
        session_id: session_id.to_string(),
        kind: "parent".to_string(),
        subagent_type: None,
        usage: parent_usage.clone(),
        tool_call_count: parent_tools.iter().map(|t| t.count).sum(),
        tools: parent_tools,
    }];

    for (child_id, subagent_type) in list_child_agents(session_dir) {
        let child_dir = find_child_session_dir(session_dir, &child_id);
        let (usage, tools) = match child_dir {
            Some(dir) => (
                read_usage(&dir.join("usage.json")),
                count_tools(&dir.join("updates.jsonl")),
            ),
            None => (ExportUsage::default(), Vec::new()),
        };
        let tool_call_count = tools.iter().map(|t| t.count).sum();
        agents.push(AgentStats {
            session_id: child_id,
            kind: "subagent".to_string(),
            subagent_type,
            usage,
            tool_call_count,
            tools,
        });
    }

    SessionStatsExport {
        format: FORMAT.to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        session_id: session_id.to_string(),
        usage: parent_usage,
        isolation,
        agents,
    }
}

fn read_usage(path: &Path) -> ExportUsage {
    let Ok(bytes) = std::fs::read(path) else {
        return ExportUsage::default();
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return ExportUsage::default();
    }
    let Ok(disk) = serde_json::from_slice::<OnDiskUsage>(&bytes) else {
        return ExportUsage::default();
    };
    disk.into_export()
}

impl OnDiskUsage {
    fn into_export(self) -> ExportUsage {
        ExportUsage {
            input_tokens: self.totals.input_tokens,
            output_tokens: self.totals.output_tokens,
            cached_read_tokens: self.totals.cached_read_tokens,
            cache_creation_tokens: self.totals.cache_creation_tokens,
            reasoning_tokens: self.totals.reasoning_tokens,
            model_calls: self.totals.model_calls,
            api_duration_ms: self.totals.api_duration_ms,
            usage_is_incomplete: self.incomplete,
            by_model: self
                .by_model
                .into_iter()
                .map(|(k, v)| (k, v.into_export()))
                .collect(),
        }
    }
}

impl OnDiskTotals {
    fn into_export(self) -> ExportUsageTotals {
        ExportUsageTotals {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_read_tokens: self.cached_read_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            reasoning_tokens: self.reasoning_tokens,
            model_calls: self.model_calls,
            api_duration_ms: self.api_duration_ms,
        }
    }
}

fn count_tools(updates_path: &Path) -> Vec<ToolStat> {
    let Ok(text) = std::fs::read_to_string(updates_path) else {
        return Vec::new();
    };
    let mut by_name: BTreeMap<String, ToolStat> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some((name, duration_ms)) = tool_call_from_update(&value) else {
            continue;
        };
        let entry = by_name.entry(name.clone()).or_insert_with(|| ToolStat {
            name,
            count: 0,
            duration_ms: 0,
        });
        entry.count = entry.count.saturating_add(1);
        entry.duration_ms = entry.duration_ms.saturating_add(duration_ms);
    }
    by_name.into_values().collect()
}

fn tool_call_from_update(value: &serde_json::Value) -> Option<(String, u64)> {
    let update = value
        .get("params")
        .and_then(|p| p.get("update"))
        .unwrap_or(value);
    let kind = update.get("sessionUpdate").and_then(|v| v.as_str())?;
    if kind != "tool_call" {
        return None;
    }
    let name = update
        .pointer("/_meta/toolName")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            update
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| update.get("kind").and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_string();
    let duration_ms = update
        .get("durationMs")
        .and_then(|v| v.as_u64())
        .or_else(|| update.get("duration_ms").and_then(|v| v.as_u64()))
        .unwrap_or(0);
    Some((name, duration_ms))
}

fn scan_isolation(chat_history_path: &Path) -> IsolationFlags {
    let Ok(text) = std::fs::read_to_string(chat_history_path) else {
        return IsolationFlags::default();
    };
    let memory_context_injected = text.contains(MEMORY_CONTEXT_TAG);
    let mut memory_tool_calls = 0u64;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        memory_tool_calls = memory_tool_calls.saturating_add(count_memory_tools(&value));
    }
    IsolationFlags {
        memory_enabled: memory_context_injected || memory_tool_calls > 0,
        memory_context_injected,
        memory_tool_calls,
    }
}

fn count_memory_tools(value: &serde_json::Value) -> u64 {
    let mut n = 0u64;
    match value {
        serde_json::Value::String(s) => {
            if s == MEMORY_SEARCH || s == MEMORY_GET {
                n = n.saturating_add(1);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                n = n.saturating_add(count_memory_tools(item));
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(name)) = map.get("name")
                && (name == MEMORY_SEARCH || name == MEMORY_GET)
            {
                n = n.saturating_add(1);
            } else {
                for v in map.values() {
                    n = n.saturating_add(count_memory_tools(v));
                }
            }
        }
        _ => {}
    }
    n
}

fn list_child_agents(parent_dir: &Path) -> Vec<(String, Option<String>)> {
    let subagents = parent_dir.join("subagents");
    let Ok(entries) = std::fs::read_dir(&subagents) else {
        return Vec::new();
    };
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let meta_path = entry.path().join("meta.json");
        let Ok(bytes) = std::fs::read(&meta_path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<SubagentMeta>(&bytes) else {
            continue;
        };
        let Some(child_id) = meta.child_session_id.filter(|s| !s.is_empty()) else {
            continue;
        };
        children.push((child_id, meta.subagent_type));
    }
    children.sort_by(|a, b| a.0.cmp(&b.0));
    children
}

fn find_child_session_dir(parent_dir: &Path, child_id: &str) -> Option<PathBuf> {
    if let Some(cwd_dir) = parent_dir.parent() {
        let same_cwd = cwd_dir.join(child_id);
        if same_cwd.is_dir() {
            return Some(same_cwd);
        }
        if let Some(sessions_root) = cwd_dir.parent() {
            if let Ok(cwds) = std::fs::read_dir(sessions_root) {
                for cwd in cwds.flatten() {
                    let candidate = cwd.path().join(child_id);
                    if candidate.is_dir() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    xai_grok_shell::session::persistence::find_session_dir_by_id(child_id)
}

#[cfg(test)]
mod export_json_tests {
    use super::*;

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    fn usage_json(input: u64, output: u64, calls: u64, incomplete: bool) -> String {
        serde_json::json!({
            "totals": {
                "input_tokens": input,
                "output_tokens": output,
                "cached_read_tokens": 1,
                "cache_creation_tokens": 0,
                "reasoning_tokens": 0,
                "model_calls": calls,
                "api_duration_ms": 9
            },
            "by_model": {
                "grok": {
                    "input_tokens": input,
                    "output_tokens": output,
                    "cached_read_tokens": 1,
                    "cache_creation_tokens": 0,
                    "reasoning_tokens": 0,
                    "model_calls": calls,
                    "api_duration_ms": 9
                }
            },
            "main_loop_model_calls": calls,
            "incomplete": incomplete
        })
        .to_string()
    }

    fn tool_update(title: &str) -> String {
        serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "s",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": title,
                    "title": title,
                    "kind": "read"
                }
            }
        })
        .to_string()
    }

    #[test]
    fn export_json_parent_and_child_do_not_double_count_root_usage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let cwd_dir = tmp.path().join("sessions").join("encoded");
        let parent_dir = cwd_dir.join("parent-sid");
        let child_dir = cwd_dir.join("child-sid");
        write(
            &parent_dir.join("usage.json"),
            &usage_json(150, 20, 3, false),
        );
        write(
            &parent_dir.join("updates.jsonl"),
            &format!("{}\n{}\n", tool_update("read_file"), tool_update("bash")),
        );
        write(
            &parent_dir.join("chat_history.jsonl"),
            r#"{"type":"user","content":"hi"}"#,
        );
        write(
            &parent_dir.join("subagents").join("sa-1").join("meta.json"),
            r#"{"child_session_id":"child-sid","subagent_type":"explore"}"#,
        );
        write(&child_dir.join("usage.json"), &usage_json(50, 8, 1, false));
        write(
            &child_dir.join("updates.jsonl"),
            &format!("{}\n", tool_update("read_file")),
        );

        let bundle = build_from_session_dir(&parent_dir, "parent-sid");
        assert_eq!(bundle.format, FORMAT);
        assert_eq!(bundle.session_id, "parent-sid");
        assert_eq!(bundle.usage.input_tokens, 150);
        assert_eq!(bundle.usage.output_tokens, 20);
        assert_eq!(bundle.usage.model_calls, 3);
        assert!(!bundle.usage.usage_is_incomplete);
        assert_eq!(bundle.agents.len(), 2);
        assert_eq!(bundle.agents[0].kind, "parent");
        assert_eq!(bundle.agents[0].tool_call_count, 2);
        assert_eq!(bundle.agents[1].kind, "subagent");
        assert_eq!(bundle.agents[1].subagent_type.as_deref(), Some("explore"));
        assert_eq!(bundle.agents[1].usage.input_tokens, 50);
        let summed_input = bundle.agents[0].usage.input_tokens
            + bundle.agents[1].usage.input_tokens;
        assert_ne!(
            summed_input, bundle.usage.input_tokens,
            "root usage is the folded parent ledger, not parent+child"
        );
        assert!(!bundle.isolation.memory_enabled);
        assert!(!bundle.isolation.memory_context_injected);
        assert_eq!(bundle.isolation.memory_tool_calls, 0);
    }

    #[test]
    fn export_json_missing_usage_file_is_zeros() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_dir = tmp.path().join("sid");
        std::fs::create_dir_all(&session_dir).unwrap();
        let bundle = build_from_session_dir(&session_dir, "sid");
        assert_eq!(bundle.usage, ExportUsage::default());
        assert_eq!(bundle.agents.len(), 1);
        assert_eq!(bundle.agents[0].tool_call_count, 0);
    }

    #[test]
    fn export_json_scans_memory_isolation_from_chat_history() {
        let tmp = tempfile::TempDir::new().unwrap();
        let session_dir = tmp.path().join("sid");
        write(
            &session_dir.join("chat_history.jsonl"),
            &format!(
                "{}\n{}\n",
                serde_json::json!({"type":"system","content": format!("{MEMORY_CONTEXT_TAG} note")}),
                serde_json::json!({"type":"assistant","tool_calls":[{"name": MEMORY_SEARCH, "id": "1"}]})
            ),
        );
        let bundle = build_from_session_dir(&session_dir, "sid");
        assert!(bundle.isolation.memory_context_injected);
        assert!(bundle.isolation.memory_tool_calls >= 1);
        assert!(bundle.isolation.memory_enabled);
    }

    #[test]
    fn export_json_expands_tilde_and_creates_parent_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let out = tmp.path().join("nested").join("run.json");
        write_json_file(&out, "{\"ok\":true}").unwrap();
        assert!(out.is_file());
        let expanded = expand_output_path(Path::new("~/exports/run.json"));
        assert!(expanded.to_string_lossy().contains("exports"));
        assert!(!expanded.to_string_lossy().starts_with('~'));
    }
}
