use crate::{CallBackend, CallPurpose, CallRecord, CallStatus, ProviderProfile};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Copy)]
pub struct RecordError;

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("provider usage checkpoint could not be saved")
    }
}
impl std::error::Error for RecordError {}

pub type RecordFuture = Pin<Box<dyn Future<Output = Result<(), RecordError>> + Send>>;

/// The session provides a weak actor-backed sink. Acknowledged records must
/// reach its atomic usage checkpoint before the future resolves successfully.
pub trait UsageSink: Send + Sync {
    fn record(&self, record: CallRecord) -> RecordFuture;
    /// Used by cancellation/drop paths, where awaiting is impossible. The
    /// previously acknowledged pending record still represents unknown spend
    /// if the process exits before this update reaches disk.
    fn record_detached(&self, record: CallRecord);
    fn report_failure(&self) {}
}

#[derive(Clone)]
pub struct UsageObserver {
    sink: Arc<dyn UsageSink>,
    session_id: String,
    prompt_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    purpose: CallPurpose,
}

impl std::fmt::Debug for UsageObserver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageObserver")
            .field("purpose", &self.purpose)
            .finish_non_exhaustive()
    }
}

impl UsageObserver {
    pub fn new(
        sink: Arc<dyn UsageSink>,
        session_id: String,
        prompt_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Self {
        Self {
            sink,
            session_id,
            prompt_id,
            purpose: CallPurpose::Auxiliary,
        }
    }

    pub fn for_purpose(&self, purpose: CallPurpose) -> Self {
        Self {
            purpose,
            ..self.clone()
        }
    }

    /// Forward the same immutable call id to the parent's session ledger.
    /// Both checkpoints are acknowledged before a child request proceeds;
    /// later background updates propagate without a one-time subagent fold.
    pub fn with_parent(mut self, parent: &Self) -> Self {
        self.sink = Arc::new(FanoutSink {
            local: self.sink,
            parent: parent.sink.clone(),
        });
        self
    }

    async fn emit(&self, record: CallRecord) -> Result<(), RecordError> {
        let result = self.sink.record(record).await;
        if result.is_err() {
            self.sink.report_failure();
        }
        result
    }
}

struct FanoutSink {
    local: Arc<dyn UsageSink>,
    parent: Arc<dyn UsageSink>,
}
impl UsageSink for FanoutSink {
    fn record(&self, record: CallRecord) -> RecordFuture {
        let local = self.local.record(record.clone());
        let parent = self.parent.record(record);
        Box::pin(async move {
            let a = local.await;
            let b = parent.await;
            a.and(b)
        })
    }
    fn record_detached(&self, record: CallRecord) {
        self.local.record_detached(record.clone());
        self.parent.record_detached(record);
    }
    fn report_failure(&self) {
        self.local.report_failure();
        self.parent.report_failure();
    }
}

/// One physical transport attempt. It starts before sending HTTP and records
/// terminal usage before returning it to a caller. Retries need a new guard.
pub struct CallGuard {
    inner: Option<(UsageObserver, CallRecord, Instant)>,
}

impl CallGuard {
    pub async fn start(
        observer: Option<&UsageObserver>,
        model: &str,
        provider: ProviderProfile,
        backend: CallBackend,
    ) -> Result<Self, RecordError> {
        let Some(observer) = observer else {
            return Ok(Self { inner: None });
        };
        let record = CallRecord {
            call_id: uuid::Uuid::new_v4().to_string(),
            origin_session_id: observer.session_id.clone(),
            prompt_id: (observer.prompt_id)(),
            model: model.to_owned(),
            provider,
            backend,
            purpose: observer.purpose,
            status: CallStatus::Pending,
            started_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            api_duration_ms: None,
            sequence: 0,
            usage: None,
        };
        // Install the guard before the await so cancellation during the
        // checkpoint attempt cannot lose the interrupted lifecycle update.
        let guard = Self {
            inner: Some((observer.clone(), record.clone(), Instant::now())),
        };
        observer.emit(record).await?;
        Ok(guard)
    }

    pub fn is_observed(&self) -> bool {
        self.inner.is_some()
    }

    pub async fn usage(&mut self, value: &serde_json::Value) -> Result<(), RecordError> {
        let Some((observer, record, started)) = &mut self.inner else {
            return Ok(());
        };
        record
            .usage
            .get_or_insert_default()
            .update(value, record.backend, record.provider);
        record.sequence = record.sequence.saturating_add(1);
        record.api_duration_ms = Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
        observer.emit(record.clone()).await
    }

    pub async fn finish(&mut self, status: CallStatus) -> Result<(), RecordError> {
        assert_ne!(
            status,
            CallStatus::Pending,
            "finish needs a terminal status"
        );
        let Some((observer, mut record, started)) = self.inner.take() else {
            return Ok(());
        };
        record.status = status;
        record.sequence = record.sequence.saturating_add(1);
        record.api_duration_ms = Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
        observer.emit(record).await
    }

    /// HTTP/decode errors are recorded even at synchronous `?` boundaries.
    pub fn failed(&mut self) {
        self.finish_detached(CallStatus::Failed);
    }

    fn finish_detached(&mut self, status: CallStatus) {
        if let Some((observer, mut record, started)) = self.inner.take() {
            record.status = status;
            record.sequence = record.sequence.saturating_add(1);
            record.api_duration_ms =
                Some(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
            observer.sink.record_detached(record);
        }
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        self.finish_detached(CallStatus::Interrupted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallLedger;
    use std::sync::Mutex;

    struct Sink(Mutex<CallLedger>);
    impl UsageSink for Sink {
        fn record(&self, record: CallRecord) -> RecordFuture {
            self.0.lock().unwrap().upsert(record);
            Box::pin(async { Ok(()) })
        }
        fn record_detached(&self, record: CallRecord) {
            self.0.lock().unwrap().upsert(record);
        }
    }
    fn observer() -> (Arc<Sink>, UsageObserver) {
        let sink = Arc::new(Sink(Mutex::new(CallLedger::new(true))));
        let observer = UsageObserver::new(
            sink.clone(),
            "synthetic-session".into(),
            Arc::new(|| Some("synthetic-prompt".into())),
        );
        (sink, observer)
    }

    #[test]
    fn drop_retains_received_usage_and_never_serializes_content() {
        let (sink, observer) = observer();
        futures::executor::block_on(async {
            let mut guard = CallGuard::start(
                Some(&observer),
                "synthetic-model",
                ProviderProfile::Openrouter,
                CallBackend::ChatCompletions,
            )
            .await
            .unwrap();
            guard
                .usage(
                    &serde_json::json!({"prompt_tokens":10,"completion_tokens":2,"cost":0.01,
                "prompt":"secret-sentinel","response":"private-response-sentinel",
                "Authorization":"credential-sentinel","url":"endpoint-sentinel"}),
                )
                .await
                .unwrap();
        });
        let ledger = sink.0.lock().unwrap();
        assert_eq!(ledger.calls()[0].status, CallStatus::Interrupted);
        assert_eq!(ledger.summary().auxiliary.cost.known_usd, Some(0.01));
        assert_eq!(ledger.summary().auxiliary.cost.total_usd, None);
        let serialized = serde_json::to_string(&*ledger).unwrap();
        assert!(!serialized.contains("sentinel"));
    }

    #[test]
    fn retries_are_distinct_and_terminal_completion_is_not_cancelled_on_drop() {
        let (sink, observer) = observer();
        let observer = observer.for_purpose(CallPurpose::MainLoop);
        futures::executor::block_on(async {
            let mut first = CallGuard::start(
                Some(&observer),
                "synthetic-model",
                ProviderProfile::Xai,
                CallBackend::Responses,
            )
            .await
            .unwrap();
            first.failed();
            let mut second = CallGuard::start(
                Some(&observer),
                "synthetic-model",
                ProviderProfile::Xai,
                CallBackend::Responses,
            )
            .await
            .unwrap();
            second
                .usage(&serde_json::json!({"input_tokens":5,"output_tokens":2,
                "cost_in_usd_ticks":20000000}))
                .await
                .unwrap();
            second.finish(CallStatus::Completed).await.unwrap();
        });
        let ledger = sink.0.lock().unwrap();
        assert_eq!(ledger.calls().len(), 2);
        assert_ne!(ledger.calls()[0].call_id, ledger.calls()[1].call_id);
        assert_eq!(ledger.calls()[0].status, CallStatus::Failed);
        assert_eq!(ledger.calls()[1].status, CallStatus::Completed);
        assert_eq!(ledger.summary().main.cost.known_usd, Some(0.002));
        assert_eq!(ledger.summary().main.cost.total_usd, None);
        assert_eq!(ledger.recording_errors, 0);
    }
    #[test]
    fn child_background_updates_reach_parent_without_double_counting() {
        let (parent_sink, parent) = observer();
        let (child_sink, child) = observer();
        let child = child
            .with_parent(&parent)
            .for_purpose(CallPurpose::SessionSummary);
        futures::executor::block_on(async {
            let mut call = CallGuard::start(
                Some(&child),
                "child-aux",
                ProviderProfile::Openrouter,
                CallBackend::ChatCompletions,
            )
            .await
            .unwrap();
            assert_eq!(
                parent_sink.0.lock().unwrap().calls()[0].status,
                CallStatus::Pending
            );
            call.usage(&serde_json::json!({"prompt_tokens":7,"completion_tokens":2,"cost":0.03}))
                .await
                .unwrap();
            call.finish(CallStatus::Completed).await.unwrap();
        });
        let child_ledger = child_sink.0.lock().unwrap().clone();
        let mut parent_ledger = parent_sink.0.lock().unwrap();
        assert_eq!(*parent_ledger, child_ledger);
        parent_ledger.merge(&child_ledger);
        assert_eq!(parent_ledger.calls().len(), 1);
        assert_eq!(parent_ledger.summary().auxiliary.cost.total_usd, Some(0.03));
        assert_eq!(parent_ledger.summary().main.model_calls, 0);
    }
}
