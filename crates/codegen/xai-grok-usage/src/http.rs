//! Content-free accounting for non-streaming model HTTP calls outside the sampler.
//! Attach the middleware AFTER auth/retry middleware so each wire retry is a
//! distinct request. General HTTP fetches and media polling do not use this.
use crate::{CallBackend, CallGuard, CallStatus, ProviderProfile, RecordError, UsageObserver};
use reqwest::ResponseBuilderExt;

#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error(transparent)]
    Checkpoint(#[from] RecordError),
    #[error(transparent)]
    Transport(#[from] reqwest::Error),
}

#[derive(Clone, Debug)]
pub struct JsonRequestObserver {
    pub observer: UsageObserver,
    pub model: String,
    pub provider: ProviderProfile,
    pub backend: CallBackend,
}

impl JsonRequestObserver {
    async fn start(&self) -> Result<CallGuard, RecordError> {
        CallGuard::start(
            Some(&self.observer),
            &self.model,
            self.provider,
            self.backend,
        )
        .await
    }

    pub async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, RequestError> {
        // Build first: a locally invalid request is not a physical attempt.
        let (client, request) = request.build_split();
        let request = request?;
        let mut call = self.start().await?;
        match client.execute(request).await {
            Ok(response) => observe_response(response, &mut call).await,
            Err(error) => {
                call.finish(CallStatus::Failed).await?;
                Err(error.into())
            }
        }
    }
}

async fn observe_response(
    response: reqwest::Response,
    call: &mut CallGuard,
) -> Result<reqwest::Response, RequestError> {
    let status = response.status();
    let mut rebuilt = http::Response::builder()
        .status(status)
        .version(response.version())
        .url(response.url().clone());
    *rebuilt.headers_mut().expect("valid response headers") = response.headers().clone();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            call.finish(CallStatus::Failed).await?;
            return Err(error.into());
        }
    };
    let value = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    if let Some(usage) = value
        .as_ref()
        .and_then(|v| v.get("usage"))
        .filter(|v| !v.is_null())
    {
        call.usage(usage).await?;
    }
    let completed = status.is_success()
        && value.as_ref().is_some_and(|v| {
            !v.get("error").is_some_and(|e| !e.is_null())
                && v.get("status").and_then(serde_json::Value::as_str) != Some("failed")
        });
    call.finish(if completed {
        CallStatus::Completed
    } else {
        CallStatus::Failed
    })
    .await?;
    Ok(reqwest::Response::from(
        rebuilt.body(bytes).expect("valid response"),
    ))
}

#[async_trait::async_trait]
impl reqwest_middleware::Middleware for JsonRequestObserver {
    async fn handle(
        &self,
        request: reqwest::Request,
        extensions: &mut http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> Result<reqwest::Response, reqwest_middleware::Error> {
        let mut call = self
            .start()
            .await
            .map_err(reqwest_middleware::Error::middleware)?;
        match next.run(request, extensions).await {
            Ok(response) => {
                observe_response(response, &mut call)
                    .await
                    .map_err(|error| match error {
                        RequestError::Checkpoint(error) => {
                            reqwest_middleware::Error::middleware(error)
                        }
                        RequestError::Transport(error) => error.into(),
                    })
            }
            Err(error) => {
                call.finish(CallStatus::Failed)
                    .await
                    .map_err(reqwest_middleware::Error::middleware)?;
                Err(error)
            }
        }
    }
}

/// Local checkpoint failures must not enter a surrounding HTTP retry loop.
pub fn is_checkpoint_error(error: &reqwest_middleware::Error) -> bool {
    matches!(error, reqwest_middleware::Error::Middleware(error) if error.is::<RecordError>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallLedger, CallPurpose, CallRecord, RecordFuture, UsageSink};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Sink {
        ledger: Mutex<CallLedger>,
        reject: AtomicBool,
    }
    impl UsageSink for Sink {
        fn record(&self, record: CallRecord) -> RecordFuture {
            if self.reject.load(Ordering::SeqCst) {
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
    fn observer() -> (Arc<Sink>, JsonRequestObserver) {
        let sink = Arc::new(Sink {
            ledger: Mutex::new(CallLedger::new(true)),
            reject: AtomicBool::new(false),
        });
        let observer = UsageObserver::new(sink.clone(), "test-session".into(), Arc::new(|| None))
            .for_purpose(CallPurpose::Embedding);
        (
            sink,
            JsonRequestObserver {
                observer,
                model: "embed-model".into(),
                provider: ProviderProfile::Openrouter,
                backend: CallBackend::Embeddings,
            },
        )
    }
    struct RetryOnce;
    #[async_trait::async_trait]
    impl reqwest_middleware::Middleware for RetryOnce {
        async fn handle(
            &self,
            request: reqwest::Request,
            extensions: &mut http::Extensions,
            next: reqwest_middleware::Next<'_>,
        ) -> Result<reqwest::Response, reqwest_middleware::Error> {
            let retry = request.try_clone().unwrap();
            let response = next.clone().run(request, extensions).await?;
            if response.status().as_u16() == 503 {
                next.run(retry, extensions).await
            } else {
                Ok(response)
            }
        }
    }

    #[tokio::test]
    async fn middleware_counts_each_inner_retry_and_preserves_response() {
        let (sink, observer) = observer();
        let hits = Arc::new(AtomicUsize::new(0));
        let server_hits = hits.clone();
        let app = axum::Router::new().route("/embeddings", axum::routing::post(move || {
            let hits = server_hits.clone();
            async move {
                let status = if hits.fetch_add(1, Ordering::SeqCst) == 0 { 503 } else { 200 };
                axum::response::Response::builder().status(status).header("x-synthetic", "retained")
                    .body(axum::body::Body::from(r#"{"data":[{"embedding":[1,2]}],"usage":{"prompt_tokens":8,"cost":0.02},"private":"not-for-ledger"}"#)).unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/embeddings", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
            .with(RetryOnce)
            .with(observer)
            .build();
        let response = client.post(&url).body("synthetic").send().await.unwrap();
        assert_eq!(response.url().as_str(), url);
        assert_eq!(response.headers()["x-synthetic"], "retained");
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["data"][0]["embedding"], serde_json::json!([1, 2]));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let ledger = sink.ledger.lock().unwrap();
        assert_eq!(ledger.calls().len(), 2);
        assert_ne!(ledger.calls()[0].call_id, ledger.calls()[1].call_id);
        assert_eq!(ledger.calls()[0].status, CallStatus::Failed);
        assert_eq!(ledger.calls()[1].status, CallStatus::Completed);
        assert_eq!(ledger.summary().auxiliary.cost.known_usd, Some(0.04));
        assert_eq!(ledger.summary().auxiliary.cost.total_usd, None);
        assert_eq!(ledger.summary().main.model_calls, 0);
        assert!(
            !serde_json::to_string(&*ledger)
                .unwrap()
                .contains("not-for-ledger")
        );
        server.abort();
    }

    #[tokio::test]
    async fn checkpoint_rejection_sends_no_request_and_does_not_retry() {
        let (sink, observer) = observer();
        sink.reject.store(true, Ordering::SeqCst);
        let hits = Arc::new(AtomicUsize::new(0));
        let server_hits = hits.clone();
        let app = axum::Router::new().route(
            "/embeddings",
            axum::routing::post(move || {
                let hits = server_hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    "{}"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/embeddings", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
            .with(RetryOnce)
            .with(observer.clone())
            .build();
        let error = client.post(&url).send().await.unwrap_err();
        assert!(is_checkpoint_error(&error));
        assert!(matches!(
            observer.send(reqwest::Client::new().post(&url)).await,
            Err(RequestError::Checkpoint(_))
        ));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert!(sink.ledger.lock().unwrap().recording_errors > 0);
        server.abort();
    }
}
