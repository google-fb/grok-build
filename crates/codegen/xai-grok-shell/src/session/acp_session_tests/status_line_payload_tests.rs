use super::support::create_test_actor;
use serde_json::Value;

// Only the string arm can fire on a built payload: every wire `Option` is
// `skip_serializing_if`.
fn assert_no_placeholders(value: &Value, path: &str) {
    match value {
        Value::String(text) => assert!(!text.is_empty(), "{path} is empty, not omitted"),
        Value::Null => panic!("{path} is null; omit the field instead"),
        Value::Object(fields) => {
            for (key, child) in fields {
                assert_no_placeholders(child, &format!("{path}.{key}"));
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                assert_no_placeholders(child, &format!("{path}[{i}]"));
            }
        }
        Value::Bool(_) | Value::Number(_) => {}
    }
}

#[tokio::test]
async fn the_payload_carries_real_values_or_no_field_at_all() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
            let actor = create_test_actor(50_000, 100_000, 85, gateway_tx, persistence_tx).await;

            let ctx = actor.build_status_context().await;
            // Two fields the payload promises are copies of another. Both are
            // built from one source today, so this fails the day one of them
            // is sourced separately and the promise quietly stops holding.
            assert_eq!(ctx.cwd, ctx.workspace.current_dir);
            if let Some(worktree) = &ctx.worktree {
                assert_eq!(worktree.branch, ctx.workspace.branch);
            }

            let value = serde_json::to_value(ctx).unwrap();
            assert_no_placeholders(&value, "payload");
            assert_eq!(value["schema_version"], 1);
        })
        .await;
}

#[tokio::test]
async fn status_line_cost_sums_provider_sources_and_hides_missing_amounts() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (gateway_tx, _) = tokio::sync::mpsc::unbounded_channel();
            let (persistence_tx, _) = tokio::sync::mpsc::unbounded_channel();
            let actor = create_test_actor(50_000, 100_000, 85, gateway_tx, persistence_tx).await;
            let usage = xai_grok_sampling_types::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 2,
                ..Default::default()
            };
            actor.chat_state_handle.record_provider_model_call_usage(
                Some("xai-model".into()),
                usage.clone(),
                None,
                Some(100_000_000),
                xai_grok_sampling_types::ProviderCost::from_xai_ticks(Some(100_000_000)),
            );
            actor.chat_state_handle.record_provider_model_call_usage(
                Some("router-model".into()),
                usage.clone(),
                None,
                None,
                Some(xai_grok_sampling_types::ProviderCost {
                    usd: 0.02,
                    source: xai_grok_sampling_types::CostSource::OpenrouterUsageCost,
                }),
            );
            assert_eq!(
                actor.build_status_context().await.cost.total_cost_usd,
                Some(0.03)
            );
            actor.chat_state_handle.record_provider_model_call_usage(
                Some("unknown-cost-model".into()),
                usage,
                None,
                None,
                None,
            );
            assert_eq!(actor.build_status_context().await.cost.total_cost_usd, None);
        })
        .await;
}
