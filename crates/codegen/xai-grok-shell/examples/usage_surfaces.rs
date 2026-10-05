//! Offline probe of the production usage projection and JSONL storage adapter.
//! Synthetic counters only: no model client, authentication, or live session.
use std::path::PathBuf;

use anyhow::{Result, bail};
use xai_chat_state::UsageLedger;
use xai_grok_sampling_types::TokenUsage;
use xai_grok_shell::extensions::notification::{
    PromptUsage, SessionNotification, SessionUpdate, attach_result_usage_fail_closed,
};
use xai_grok_shell::session::info::Info;
use xai_grok_shell::session::storage::{
    SessionUpdate as StoredUpdate, StorageAdapter, jsonl::JsonlStorageAdapter,
};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        bail!(
            "usage: usage_surfaces NEW_DIRECTORY normal|timeout|first-checkpoint-timeout|missing-cost|incomplete"
        );
    }
    let mode = args[1].as_str();
    if !matches!(
        mode,
        "normal" | "timeout" | "first-checkpoint-timeout" | "missing-cost" | "incomplete"
    ) {
        bail!("unknown mode");
    }
    let root = PathBuf::from(&args[0]);
    // Refuse reuse so the probe cannot read or overwrite any existing session.
    std::fs::create_dir(&root)?;
    let root = root.canonicalize()?;
    let adapter = JsonlStorageAdapter::with_root(root.join("sessions"));
    let info = Info {
        id: "synthetic-usage-session".into(),
        cwd: root.to_string_lossy().into_owned(),
    };
    adapter.init_session(&info, "model-a".into()).await?;
    let mut ledger = UsageLedger::default();
    let first = TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 20,
        total_tokens: 120,
        reasoning_tokens: 7,
        cached_prompt_tokens: 30,
        cache_creation_prompt_tokens: 5,
    };
    ledger.record_main_loop_call("model-a", &first, Some(12), Some(1_000_000_000));
    adapter.write_usage(&info, &ledger).await?;
    if mode == "first-checkpoint-timeout" {
        std::fs::write(root.join("ready"), "first-checkpoint")?;
        std::future::pending::<()>().await;
    }
    let second = TokenUsage {
        prompt_tokens: 60,
        completion_tokens: 10,
        total_tokens: 70,
        reasoning_tokens: 3,
        cached_prompt_tokens: 20,
        cache_creation_prompt_tokens: 0,
    };
    let second_cost = (mode != "missing-cost").then_some(2_000_000_000);
    ledger.record_main_loop_call("model-b", &second, Some(9), second_cost);
    ledger.incomplete = mode == "incomplete";
    adapter.write_usage(&info, &ledger).await?;
    if mode == "timeout" {
        std::fs::write(root.join("ready"), "second-checkpoint")?;
        std::future::pending::<()>().await;
    }
    let usage = PromptUsage::from(&ledger);
    adapter
        .append_update(
            &info,
            &StoredUpdate::Xai(Box::new(SessionNotification {
                session_id: info.id.clone(),
                update: SessionUpdate::TurnCompleted {
                    prompt_id: "synthetic-prompt".into(),
                    stop_reason: "end_turn".into(),
                    agent_result: None,
                    error_kind: None,
                    usage: Some(usage.clone()),
                    elapsed_ms: Some(21),
                },
                meta: None,
            })),
        )
        .await?;
    let mut terminal = serde_json::json!({"stopReason": "end_turn"});
    attach_result_usage_fail_closed(&mut terminal, &serde_json::to_value(usage)?);
    std::fs::write(
        root.join("terminal.json"),
        serde_json::to_vec_pretty(&terminal)?,
    )?;
    Ok(())
}
