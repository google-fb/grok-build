use super::*;
use crate::session::info::Info;
use crate::session::storage::jsonl::JsonlStorageAdapter;
use crate::session::storage::{StorageAdapter, USAGE_FILE};
use agent_client_protocol as acp;
use std::sync::Arc;
use xai_chat_state::{ChatStateActor, NullChatPersistence, UsageLedger, UsageTotals};
use xai_grok_sampling_types::TokenUsage;

fn test_info() -> Info {
    Info {
        id: acp::SessionId::new("usage-persist-resume"),
        cwd: "/test/workspace".into(),
    }
}

fn sample_ledger() -> UsageLedger {
    let mut ledger = UsageLedger::default();
    let first = TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 10,
        total_tokens: 0,
        reasoning_tokens: 0,
        cached_prompt_tokens: 4,
        cache_creation_prompt_tokens: 0,
    };
    let second = TokenUsage {
        prompt_tokens: 50,
        completion_tokens: 5,
        total_tokens: 0,
        reasoning_tokens: 0,
        cached_prompt_tokens: 2,
        cache_creation_prompt_tokens: 0,
    };
    ledger.record_main_loop_call("parent-model", &first, Some(20), None);
    ledger.record_main_loop_call("parent-model", &second, Some(10), None);
    ledger.record_subagent(
        &[(
            "child-model".into(),
            UsageTotals {
                input_tokens: 7,
                output_tokens: 3,
                cached_read_tokens: 1,
                model_calls: 1,
                ..Default::default()
            },
        )],
        true,
    );
    ledger
}

fn sampling_config() -> xai_grok_sampling_types::SamplingConfig {
    xai_grok_sampling_types::SamplingConfig {
        provider_profile: Some(xai_grok_sampling_types::ProviderProfile::Xai),
        base_url: String::new(),
        model: "parent-model".into(),
        max_completion_tokens: None,
        temperature: None,
        top_p: None,
        api_backend: Default::default(),
        extra_headers: Default::default(),
        query_params: Default::default(),
        env_http_headers: Default::default(),
        context_window: std::num::NonZeroU64::new(128_000).unwrap(),
        reasoning_effort: None,
        stream_tool_calls: None,
    }
}

async fn restore_into_actor(ledger: UsageLedger) -> UsageLedger {
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = ChatStateActor::spawn_with_session_usage(
        vec![],
        sampling_config(),
        xai_chat_state::PruningConfig::default(),
        Box::new(NullChatPersistence),
        event_tx,
        tokio_util::sync::CancellationToken::new(),
        ledger,
    );
    handle
        .try_get_session_usage()
        .await
        .expect("chat-state actor alive")
}

#[tokio::test]
async fn usage_persist_resume_roundtrip_restores_ledger() {
    let temp = tempfile::TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp.path().to_path_buf());
    let info = test_info();
    adapter
        .init_session(&info, crate::session::persistence::default_model_id())
        .await
        .unwrap();

    let ledger = sample_ledger();
    adapter.write_usage(&info, &ledger).await.unwrap();

    let usage_path = adapter
        .load_session(&info)
        .await
        .unwrap()
        .usage
        .expect("usage.json present");
    assert_eq!(usage_path.totals.input_tokens, 157);
    assert_eq!(usage_path.totals.output_tokens, 18);
    assert_eq!(usage_path.totals.cached_read_tokens, 7);
    assert_eq!(usage_path.totals.model_calls, 3);
    assert_eq!(usage_path.main_loop_model_calls, 2);
    assert!(usage_path.incomplete);

    let restored = restore_into_actor(usage_path.clone()).await;
    assert_eq!(restored.totals.input_tokens, ledger.totals.input_tokens);
    assert_eq!(restored.totals.output_tokens, ledger.totals.output_tokens);
    assert_eq!(
        restored.totals.cached_read_tokens,
        ledger.totals.cached_read_tokens
    );
    assert_eq!(restored.totals.model_calls, ledger.totals.model_calls);
    assert_eq!(restored.main_loop_model_calls, ledger.main_loop_model_calls);
    assert_eq!(restored.incomplete, ledger.incomplete);
    assert_eq!(restored, ledger);
}

#[tokio::test]
async fn usage_persist_resume_missing_file_loads_empty() {
    let temp = tempfile::TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp.path().to_path_buf());
    let info = test_info();
    adapter
        .init_session(&info, crate::session::persistence::default_model_id())
        .await
        .unwrap();

    let loaded = adapter.load_session(&info).await.unwrap();
    assert!(loaded.usage.is_none(), "old sessions have no usage.json");

    let light = adapter.load_session_without_updates(&info).await.unwrap();
    assert!(light.usage.is_none());

    let restored = restore_into_actor(UsageLedger::default()).await;
    assert_eq!(restored, UsageLedger::default());
}

#[tokio::test]
async fn usage_persist_resume_crash_keeps_first_call_only() {
    let temp = tempfile::TempDir::new().unwrap();
    let adapter = Arc::new(JsonlStorageAdapter::with_root(temp.path().to_path_buf()));
    let info = test_info();
    adapter
        .init_session(&info, crate::session::persistence::default_model_id())
        .await
        .unwrap();

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (disk_full_tx, _disk_full_rx) = tokio::sync::watch::channel(false);
    let sampling_client =
        OaiCompatClient::new(xai_grok_sampler::SamplerConfig::default()).unwrap();
    let summary =
        crate::session::summary::SummaryGenerator::new(crate::session::summary::SummaryConfig {
            sampling_client,
            model: String::new(),
            persistence_tx: tx.downgrade(),
        });
    let actor_info = info.clone();
    let storage = adapter.clone();
    let task = tokio::spawn(
        SessionPersistence {
            info: actor_info,
            storage,
            pending_notification: None,
            rx,
            remote_sync: None,
            created_fresh: false,
            relay_sync: None,
            summary,
            registry_title_sync: None,
            gateway: None,
            search_index: crate::session::storage::search::SharedSearchIndex::never_indexed(),
            disk_full_tx,
            disk_full_notified: false,
            dirty_files: Default::default(),
            pending_write_error: None,
        }
        .run(),
    );

    let mut first = UsageLedger::default();
    first.record_main_loop_call(
        "m",
        &TokenUsage {
            prompt_tokens: 11,
            completion_tokens: 2,
            total_tokens: 0,
            reasoning_tokens: 0,
            cached_prompt_tokens: 1,
            cache_creation_prompt_tokens: 0,
        },
        Some(5),
        None,
    );
    tx.send(PersistenceMsg::Usage(first.clone())).unwrap();
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    tx.send(PersistenceMsg::FlushAndAck { respond_to: ack_tx })
        .unwrap();
    ack_rx.await.unwrap().unwrap();

    let on_disk = adapter
        .load_session(&info)
        .await
        .unwrap()
        .usage
        .expect("first call persisted");
    assert_eq!(on_disk.totals.input_tokens, 11);
    assert_eq!(on_disk.main_loop_model_calls, 1);

    drop(tx);
    task.abort();
    let _ = task.await;

    let restored = restore_into_actor(on_disk).await;
    assert_eq!(restored.totals.input_tokens, 11);
    assert_eq!(restored.totals.output_tokens, 2);
    assert_eq!(restored.totals.cached_read_tokens, 1);
    assert_eq!(restored.totals.model_calls, 1);
    assert_eq!(restored.main_loop_model_calls, 1);
    assert!(!restored.incomplete);
    assert!(
        !temp
            .path()
            .join("sessions")
            .exists()
            || restored.totals.input_tokens != 20
    );
}

#[tokio::test]
async fn usage_persist_resume_writes_usage_json_next_to_signals() {
    let temp = tempfile::TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp.path().to_path_buf());
    let info = test_info();
    adapter
        .init_session(&info, crate::session::persistence::default_model_id())
        .await
        .unwrap();
    adapter
        .write_usage(&info, &sample_ledger())
        .await
        .unwrap();

    let loaded = adapter.load_session(&info).await.unwrap();
    let session_dir = temp
        .path()
        .join("sessions")
        .join(crate::util::grok_home::encode_cwd_dirname(&info.cwd))
        .join(info.id.0.as_ref());
    assert!(session_dir.join(USAGE_FILE).is_file());
    assert_eq!(
        loaded.usage.as_ref().map(|u| u.totals.input_tokens),
        Some(157)
    );
}
