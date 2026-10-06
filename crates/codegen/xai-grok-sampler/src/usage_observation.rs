//! Observe provider reports before typed decoding/defaults or UI context-window
//! rewrites. Only the usage object reaches the content-free accounting sink.
use eventsource_stream::Event;
use futures_util::{Stream, StreamExt, stream::BoxStream};
use serde_json::Value;
use xai_grok_sampling_types::{Result, SamplingError};
use xai_grok_usage::{CallBackend, CallGuard, CallStatus};

fn checkpoint(_: xai_grok_usage::RecordError) -> SamplingError {
    SamplingError::UsageCheckpoint
}

pub(crate) async fn finish_request<T>(call: Option<CallGuard>, result: Result<T>) -> Result<T> {
    if let Some(mut call) = call {
        call.finish(if result.is_ok() {
            CallStatus::Completed
        } else {
            CallStatus::Failed
        })
        .await
        .map_err(checkpoint)?;
    }
    result
}

fn usage(value: &Value) -> Option<&Value> {
    value
        .get("usage")
        .or_else(|| value.pointer("/response/usage"))
        .or_else(|| value.pointer("/message/usage"))
        .filter(|value| !value.is_null())
}

pub(crate) async fn observe_body(call: &mut CallGuard, bytes: &[u8]) -> Result<()> {
    if !call.is_observed() {
        return Ok(());
    }
    // The existing typed decoder still owns payload validation and errors.
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        if let Some(usage) = usage(&value) {
            call.usage(usage).await.map_err(checkpoint)?;
        }
        if value.get("error").is_some_and(|v| !v.is_null())
            || value.get("status").and_then(Value::as_str) == Some("failed")
        {
            call.finish(CallStatus::Failed).await.map_err(checkpoint)?;
        }
    }
    Ok(())
}

pub(crate) fn observe_stream<S, E>(
    events: S,
    mut call: CallGuard,
    backend: CallBackend,
) -> BoxStream<'static, Result<Event>>
where
    S: Stream<Item = std::result::Result<Event, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    async_stream::stream! {
        let mut events = Box::pin(events);
        let mut chat_finished = false;
        while let Some(event) = events.next().await {
            let event = match event {
                Ok(event) => event,
                Err(error) => {
                    match call.finish(CallStatus::Failed).await {
                        Ok(()) => yield Err(SamplingError::EventStreamError(error.to_string())),
                        Err(error) => yield Err(checkpoint(error)),
                    }
                    return;
                }
            };
            if call.is_observed() {
                let status = if event.data == "[DONE]" {
                    Some(if backend == CallBackend::ChatCompletions {
                        CallStatus::Completed
                    } else {
                        // Responses/Messages have explicit terminal events.
                        CallStatus::Interrupted
                    })
                } else {
                    match serde_json::from_str::<Value>(&event.data) {
                        Ok(value) => {
                            if let Some(usage) = usage(&value) {
                                if let Err(error) = call.usage(usage).await {
                                    yield Err(checkpoint(error));
                                    return;
                                }
                            }
                            let kind = value.get("type").and_then(Value::as_str)
                                .unwrap_or(&event.event);
                            if value.get("error").is_some_and(|v| !v.is_null())
                                || matches!(kind, "error" | "response.failed") {
                                Some(CallStatus::Failed)
                            } else {
                                match backend {
                                    CallBackend::Responses if matches!(kind, "response.completed" | "response.incomplete") => Some(CallStatus::Completed),
                                    CallBackend::Messages if kind == "message_stop" => Some(CallStatus::Completed),
                                    CallBackend::ChatCompletions => {
                                        chat_finished |= value.get("choices").and_then(Value::as_array)
                                            .is_some_and(|choices| choices.iter().any(|choice| choice.get("finish_reason").is_some_and(|v| !v.is_null())));
                                        None
                                    }
                                    _ => None,
                                }
                            }
                        }
                        // Preserve the existing decoder's diagnostic, but retain
                        // a failed request with any usage received before it.
                        Err(_) => Some(CallStatus::Failed),
                    }
                };
                if let Some(status) = status {
                    if let Err(error) = call.finish(status).await {
                        yield Err(checkpoint(error));
                        return;
                    }
                }
            }
            yield Ok(event);
        }
        // A Chat Completions server may close after the finish/usage chunks
        // without [DONE]. Never infer completion merely from a clean EOF.
        if call.is_observed() {
            let status = if backend == CallBackend::ChatCompletions && chat_finished {
                CallStatus::Completed
            } else {
                CallStatus::Interrupted
            };
            if let Err(error) = call.finish(status).await {
                yield Err(checkpoint(error));
            }
        }
    }.boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use xai_grok_usage::{
        CallLedger, CallPurpose, CallRecord, ProviderProfile, RecordError, RecordFuture,
        UsageObserver, UsageSink,
    };

    struct Sink {
        ledger: Mutex<CallLedger>,
        reject: Mutex<bool>,
    }
    impl UsageSink for Sink {
        fn record(&self, record: CallRecord) -> RecordFuture {
            if *self.reject.lock().unwrap() {
                return Box::pin(async { Err(RecordError) });
            }
            self.ledger.lock().unwrap().upsert(record);
            Box::pin(async { Ok(()) })
        }
        fn record_detached(&self, record: CallRecord) {
            self.ledger.lock().unwrap().upsert(record);
        }
        fn report_failure(&self) {
            self.ledger.lock().unwrap().recording_errors += 1;
        }
    }
    async fn setup(backend: CallBackend, provider: ProviderProfile) -> (Arc<Sink>, CallGuard) {
        let sink = Arc::new(Sink {
            ledger: Mutex::new(CallLedger::new(true)),
            reject: Mutex::new(false),
        });
        let observer = UsageObserver::new(
            sink.clone(),
            "session".into(),
            Arc::new(|| Some("prompt".into())),
        )
        .for_purpose(CallPurpose::MainLoop);
        let guard = CallGuard::start(Some(&observer), "model", provider, backend)
            .await
            .unwrap();
        (sink, guard)
    }
    fn events(
        items: &[&str],
    ) -> impl Stream<Item = std::result::Result<Event, &'static str>> + Send + 'static {
        futures_util::stream::iter(
            items
                .iter()
                .map(|data| {
                    Ok(Event {
                        data: (*data).into(),
                        ..Default::default()
                    })
                })
                .collect::<Vec<_>>(),
        )
    }

    #[tokio::test]
    async fn chat_cumulative_usage_is_saved_once_and_explicit_zero_cost_survives() {
        let (sink, call) = setup(CallBackend::ChatCompletions, ProviderProfile::Openrouter).await;
        let items = [
            r#"{"choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"cost":0.1}}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":3,"cost":0}}"#,
            "[DONE]",
        ];
        let got = observe_stream(events(&items), call, CallBackend::ChatCompletions)
            .collect::<Vec<_>>()
            .await;
        assert!(got.iter().all(Result::is_ok));
        let ledger = sink.ledger.lock().unwrap();
        assert_eq!(ledger.calls().len(), 1);
        assert_eq!(ledger.calls()[0].status, CallStatus::Completed);
        assert_eq!(ledger.summary().all.total_tokens.total, Some(13));
        assert_eq!(ledger.summary().all.cost.total_usd, Some(0.0));
    }

    #[tokio::test]
    async fn responses_preserve_billable_input_before_context_window_override() {
        let (sink, call) = setup(CallBackend::Responses, ProviderProfile::Xai).await;
        let terminal = r#"{"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":7,"total_tokens":107,"cost_in_usd_ticks":10000000,"context_window_details":{"input_tokens":4}}}}"#;
        let mut stream = observe_stream(events(&[terminal]), call, CallBackend::Responses);
        assert!(stream.next().await.unwrap().is_ok());
        // Consumers can drop immediately upon terminal delivery: the terminal
        // checkpoint must already be acknowledged, without another poll.
        drop(stream);
        let ledger = sink.ledger.lock().unwrap();
        assert_eq!(ledger.calls()[0].status, CallStatus::Completed);
        assert_eq!(ledger.summary().all.total_tokens.total, Some(107));
        assert_eq!(ledger.summary().all.cost.total_usd, Some(0.001));
    }

    #[tokio::test]
    async fn messages_preserve_start_input_and_final_output_without_adding_snapshots() {
        let (sink, call) = setup(CallBackend::Messages, ProviderProfile::Compatible).await;
        let items = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":8,"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"output_tokens":1}}}"#,
            r#"{"type":"message_delta","usage":{"output_tokens":5}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        observe_stream(events(&items), call, CallBackend::Messages)
            .collect::<Vec<_>>()
            .await;
        let ledger = sink.ledger.lock().unwrap();
        let usage = ledger.calls()[0].usage.as_ref().unwrap();
        assert_eq!(usage.input_tokens, Some(13));
        assert_eq!(usage.uncached_input_tokens, Some(8));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(ledger.summary().all.total_tokens.total, Some(18));
        assert_eq!(ledger.summary().all.cost.total_usd, None);
    }

    #[tokio::test]
    async fn cancellation_and_clean_truncation_retain_partial_usage() {
        for early_drop in [true, false] {
            let (sink, call) = setup(CallBackend::Responses, ProviderProfile::Xai).await;
            let mut stream = observe_stream(
                events(&[
                    r#"{"type":"response.created","response":{"usage":{"input_tokens":12}}}"#,
                ]),
                call,
                CallBackend::Responses,
            );
            assert!(stream.next().await.unwrap().is_ok());
            if !early_drop {
                assert!(stream.next().await.is_none());
            }
            drop(stream);
            let ledger = sink.ledger.lock().unwrap();
            assert_eq!(ledger.calls()[0].status, CallStatus::Interrupted);
            assert_eq!(
                ledger.calls()[0].usage.as_ref().unwrap().input_tokens,
                Some(12)
            );
            assert_eq!(ledger.summary().all.input_tokens.total, None);
            assert_eq!(ledger.summary().all.input_tokens.known_total, Some(12));
        }
    }

    #[tokio::test]
    async fn failed_stream_and_malformed_payload_are_not_completed() {
        for data in [
            r#"{"type":"response.failed","response":{"usage":{"input_tokens":3,"output_tokens":1}}}"#,
            "invalid json",
        ] {
            let (sink, call) = setup(CallBackend::Responses, ProviderProfile::Xai).await;
            observe_stream(events(&[data]), call, CallBackend::Responses)
                .collect::<Vec<_>>()
                .await;
            assert_eq!(
                sink.ledger.lock().unwrap().calls()[0].status,
                CallStatus::Failed
            );
        }
        let (sink, call) = setup(CallBackend::Messages, ProviderProfile::Compatible).await;
        let got = observe_stream(
            futures_util::stream::iter([Err::<Event, _>("transport failed")]),
            call,
            CallBackend::Messages,
        )
        .collect::<Vec<_>>()
        .await;
        assert_eq!(got.len(), 1);
        assert!(matches!(&got[0], Err(SamplingError::EventStreamError(_))));
        assert_eq!(
            sink.ledger.lock().unwrap().calls()[0].status,
            CallStatus::Failed
        );
    }

    #[tokio::test]
    async fn checkpoint_failure_remains_nonretryable_and_keeps_durable_pending_record() {
        let (sink, call) = setup(CallBackend::ChatCompletions, ProviderProfile::Openrouter).await;
        *sink.reject.lock().unwrap() = true;
        let got = observe_stream(
            events(&[r#"{"usage":{"prompt_tokens":9}}"#]),
            call,
            CallBackend::ChatCompletions,
        )
        .collect::<Vec<_>>()
        .await;
        assert_eq!(got.len(), 1);
        let error = got.into_iter().next().unwrap().unwrap_err();
        assert!(matches!(error, SamplingError::UsageCheckpoint));
        assert!(!error.is_retryable());
        assert!(!crate::retry::clone_error(&error).is_retryable());
        assert!(sink.ledger.lock().unwrap().recording_errors > 0);
    }

    #[tokio::test]
    async fn nonstream_body_missing_counters_stay_unknown_and_failure_retains_usage() {
        let (sink, mut call) = setup(CallBackend::Responses, ProviderProfile::Xai).await;
        observe_body(&mut call, br#"{"status":"failed","usage":{"input_tokens":0,"cost_in_usd_ticks":0},"prompt":"private-sentinel"}"#).await.unwrap();
        finish_request(Some(call), Ok(())).await.unwrap();
        let ledger = sink.ledger.lock().unwrap();
        let record = &ledger.calls()[0];
        assert_eq!(record.status, CallStatus::Failed);
        assert_eq!(record.usage.as_ref().unwrap().input_tokens, Some(0));
        assert_eq!(record.usage.as_ref().unwrap().output_tokens, None);
        assert_eq!(ledger.summary().all.cost.total_usd, None);
        assert!(
            !serde_json::to_string(&*ledger)
                .unwrap()
                .contains("private-sentinel")
        );
    }
    #[tokio::test]
    async fn six_http_entrypoints_ack_start_and_capture_raw_usage() {
        use crate::{ApiBackend, SamplerConfig, SamplingClient};
        use axum::{Router, extract::Json, http::Uri, routing::post};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use xai_grok_sampling_types::{
            ChatCompletionRequest, CreateResponseWrapper, MessagesRequestWrapper, messages, rs,
        };
        for backend in [
            CallBackend::ChatCompletions,
            CallBackend::Responses,
            CallBackend::Messages,
        ] {
            for streaming in [false, true] {
                let sink = Arc::new(Sink {
                    ledger: Mutex::new(CallLedger::new(true)),
                    reject: Mutex::new(false),
                });
                let observer = UsageObserver::new(
                    sink.clone(),
                    "http-session".into(),
                    Arc::new(|| Some("http-prompt".into())),
                )
                .for_purpose(CallPurpose::MainLoop);
                let hits = Arc::new(AtomicUsize::new(0));
                let server_sink = sink.clone();
                let server_hits = hits.clone();
                let app = Router::new().fallback(post(move |uri: Uri, Json(body): Json<Value>| {
                    let sink = server_sink.clone();
                    let hits = server_hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(sink.ledger.lock().unwrap().calls()[0].status, CallStatus::Pending,
                            "acknowledged pending checkpoint must precede HTTP");
                        assert_eq!(body["model"], "model");
                        assert_eq!(body.get("stream").and_then(Value::as_bool).unwrap_or(false), streaming);
                        let path = uri.path();
                        let usage = if path.ends_with("chat/completions") {
                            serde_json::json!({"prompt_tokens":10,"completion_tokens":3,"total_tokens":13,"cost":0.02,
                                "prompt_tokens_details":{"cached_tokens":2},"completion_tokens_details":{"reasoning_tokens":1}})
                        } else {
                            serde_json::json!({"input_tokens":10,"output_tokens":3,"total_tokens":13,"cost":0.02,
                                "input_tokens_details":{"cached_tokens":2},"output_tokens_details":{"reasoning_tokens":1},
                                "cache_read_input_tokens":0,"cache_creation_input_tokens":0})
                        };
                        let response = if path.ends_with("chat/completions") {
                            serde_json::json!({"id":"c","object":if streaming {"chat.completion.chunk"} else {"chat.completion"},"created":0,"model":"model","choices":[],"usage":usage})
                        } else if path.ends_with("responses") {
                            serde_json::json!({"id":"r","object":"response","created_at":0,"model":"model","status":"completed","output":[],"usage":usage})
                        } else {
                            serde_json::json!({"id":"m","type":"message","role":"assistant","model":"model","content":[],"stop_reason":"end_turn","usage":usage})
                        };
                        let payload = if !streaming { response.to_string() }
                        else if path.ends_with("chat/completions") { format!("data: {response}\n\ndata: [DONE]\n\n") }
                        else if path.ends_with("responses") { format!("data: {}\n\n", serde_json::json!({"type":"response.completed","sequence_number":1,"response":response})) }
                        else { format!("data: {}\n\ndata: {}\n\n", serde_json::json!({"type":"message_start","message":response}), serde_json::json!({"type":"message_stop"})) };
                        axum::response::Response::builder()
                            .header("content-type", if streaming { "text/event-stream" } else { "application/json" })
                            .body(axum::body::Body::from(payload)).unwrap()
                    }
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    axum::serve(listener, app).await.unwrap();
                });
                let client = SamplingClient::new(SamplerConfig {
                    base_url: format!("http://{address}/v1"),
                    model: "model".into(),
                    api_backend: match backend {
                        CallBackend::Responses => ApiBackend::Responses,
                        CallBackend::Messages => ApiBackend::Messages,
                        _ => ApiBackend::ChatCompletions,
                    },
                    provider_profile: ProviderProfile::Openrouter,
                    usage_observer: Some(observer),
                    ..Default::default()
                })
                .unwrap();
                match backend {
                    CallBackend::ChatCompletions => {
                        let req = ChatCompletionRequest::new("model", vec![]);
                        if streaming {
                            let (stream, _) = client.chat_completion_stream(req).await.unwrap();
                            assert!(
                                stream
                                    .collect::<Vec<_>>()
                                    .await
                                    .into_iter()
                                    .all(|x| x.is_ok())
                            );
                        } else {
                            client.chat_completion(req).await.unwrap();
                        }
                    }
                    CallBackend::Responses => {
                        let req = CreateResponseWrapper::new(rs::CreateResponse {
                            input: rs::InputParam::Text("synthetic".into()),
                            ..Default::default()
                        });
                        if streaming {
                            let (stream, _, _) = client.create_response_stream(req).await.unwrap();
                            assert!(
                                stream
                                    .collect::<Vec<_>>()
                                    .await
                                    .into_iter()
                                    .all(|x| x.is_ok())
                            );
                        } else {
                            client.create_response(req).await.unwrap();
                        }
                    }
                    CallBackend::Messages => {
                        let req = MessagesRequestWrapper::new(messages::MessagesRequest {
                            max_tokens: 8,
                            ..Default::default()
                        });
                        if streaming {
                            let (stream, _) = client.create_message_stream(req).await.unwrap();
                            assert!(
                                stream
                                    .collect::<Vec<_>>()
                                    .await
                                    .into_iter()
                                    .all(|x| x.is_ok())
                            );
                        } else {
                            client.create_message(req).await.unwrap();
                        }
                    }
                    _ => unreachable!(),
                }
                server.abort();
                assert_eq!(hits.load(Ordering::SeqCst), 1);
                let ledger = sink.ledger.lock().unwrap();
                assert_eq!(ledger.calls().len(), 1);
                assert_eq!(
                    ledger.calls()[0].status,
                    CallStatus::Completed,
                    "{backend:?}, streaming={streaming}"
                );
                assert_eq!(ledger.summary().main.total_tokens.total, Some(13));
                assert_eq!(ledger.summary().main.cost.total_usd, Some(0.02));
                assert_eq!(ledger.summary().auxiliary.model_calls, 0);
            }
        }
    }
}
