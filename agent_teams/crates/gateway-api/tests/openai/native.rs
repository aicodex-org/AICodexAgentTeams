//! 三协议原生入口的鉴权、wire 保真与真实 Core 交付结算回归

use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use gateway_core::{
    engine::AttemptContext,
    engine::provider::{Provider, ProviderCallMetadata, ProviderRequest, ProviderStream},
    error::ProviderError,
    event::{GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta},
    metering::Usage,
    operation::Operation,
    routing::{ProviderCatalogGeneration, ProviderModelCapabilities},
    upstream::UpstreamTransport,
};
use serde_json::{Value, json};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;

#[derive(Default)]
struct Ports {
    finalizations: Mutex<Vec<ModelRequestFinalization>>,
    active: AtomicUsize,
}

#[async_trait]
impl ExecutionStore for Ports {
    async fn create_model_request(&self, _: NewModelRequest) -> Result<(), StoreError> {
        Ok(())
    }
    async fn record_attempt(&self, _: AttemptRecord) -> Result<(), StoreError> {
        Ok(())
    }
    async fn mark_send_state(
        &self,
        _: &ModelRequestId,
        _: UpstreamSendState,
    ) -> Result<(), StoreError> {
        Ok(())
    }
    async fn mark_downstream_committed(
        &self,
        _: &ModelRequestId,
        _: SystemTime,
        _: Option<u16>,
    ) -> Result<(), StoreError> {
        Ok(())
    }
    async fn record_client_status(&self, _: &ModelRequestId, _: u16) -> Result<(), StoreError> {
        Ok(())
    }
    async fn record_intermediate_failure(&self, _: IntermediateFailure) -> Result<(), StoreError> {
        Ok(())
    }
    async fn finalize_model_request(
        &self,
        finalization: ModelRequestFinalization,
    ) -> Result<(), StoreError> {
        self.finalizations.lock().unwrap().push(finalization);
        Ok(())
    }
    async fn recover_expired(&self, _: SystemTime) -> Result<RecoveryReport, StoreError> {
        unreachable!()
    }
}

impl ClientAdmissionPort for Ports {
    fn abandon(&self, _: &ClientApiKeyId, _: &ModelRequestId) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
    fn admit(
        &self,
        _: ClientAdmissionRequest,
    ) -> BoxFuture<'_, Result<ClientAdmissionDecision, ClientAdmissionError>> {
        Box::pin(async {
            self.active.fetch_add(1, Ordering::SeqCst);
            Ok(ClientAdmissionDecision::Granted)
        })
    }
    fn release<'a>(
        &'a self,
        _: &'a ClientApiKeyId,
        _: &'a ModelRequestId,
    ) -> BoxFuture<'a, Result<bool, ClientAdmissionError>> {
        Box::pin(async {
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(true)
        })
    }
    fn restore(
        &self,
        _: ClientAdmissionRecovery,
    ) -> BoxFuture<'_, Result<ClientAdmissionRestoreResult, ClientAdmissionError>> {
        Box::pin(async { unreachable!() })
    }
}

#[derive(Clone, Copy)]
enum Reply {
    Normal,
    MissingTerminal,
    WrongProtocol,
    TerminalOnly,
    Pending,
    UpstreamError,
}

struct NativeProvider {
    captures: Mutex<Vec<Operation>>,
    reply: Reply,
}

#[async_trait]
impl Provider for NativeProvider {
    fn name(&self) -> &str {
        "deepseek"
    }
    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }
    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(vec![])
    }
    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        _: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let operation = request.operation().clone();
        self.captures.lock().unwrap().push(operation.clone());
        let Operation::Generate(generate) = &operation else {
            unreachable!()
        };
        let protocol = if matches!(self.reply, Reply::WrongProtocol) {
            "wrong"
        } else {
            operation.protocol()
        };
        let metadata = ProviderCallMetadata::new(
            request.candidate().provider().clone(),
            request.candidate().upstream_model().unwrap().clone(),
            ProviderAccountId::new("acct_api_test").unwrap(),
            UpstreamTransport::new("http").unwrap(),
        );
        if matches!(self.reply, Reply::UpstreamError) {
            let error = ProviderError::new(gateway_core::error::ProviderErrorKind::InvalidRequest, UpstreamSendState::Sent)
                .with_status(422)
                .with_client_visible_upstream_response(gateway_core::error::ClientVisibleUpstreamResponse::new(
                    422, Some(b"application/json".to_vec()), bytes::Bytes::from_static(br#"{ "error":{"type":"invalid_request_error","message":"upstream validation"}, "future":9007199254740993 }"#)));
            return Ok(ProviderStream::new(
                metadata,
                futures::stream::iter([Err(error)]),
                (),
            ));
        }
        let meta = ResponseMeta::new("native_response", "model-a");
        let completed = vec![
            GatewayEvent::Usage(Usage {
                input_tokens: Some(12),
                output_tokens: Some(3),
                ..Usage::new()
            }),
            GatewayEvent::Completed(meta.clone()),
        ];
        let streaming = generate
            .protocol_payload()
            .body()
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let events = if streaming {
            let mut events = vec![];
            if !matches!(self.reply, Reply::TerminalOnly) {
                events.push(ProviderEvent::canonical_with_wire(
                    vec![GatewayEvent::Started(meta)],
                    ProtocolWireEvent::raw_sse(
                        protocol,
                        "data: {\"delta\":\"hello\",\"future\":9007199254740993}\n\n".into(),
                    )
                    .unwrap(),
                ));
            }
            if !matches!(self.reply, Reply::MissingTerminal | Reply::Pending) {
                let terminal = if operation.protocol() == "anthropic" {
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
                } else {
                    "data: [DONE]\n\n"
                };
                events.push(ProviderEvent::canonical_with_wire(
                    completed,
                    ProtocolWireEvent::raw_sse(protocol, terminal.into()).unwrap(),
                ));
            }
            events
        } else {
            vec![ProviderEvent::canonical_with_wire(completed, ProtocolWireEvent::raw_json(protocol,
                "{ \"id\": \"native_response\", \"future\":9007199254740993, \"content\":\"hello\" }".into()).unwrap())]
        };
        use futures::StreamExt as _;
        let stream = futures::stream::iter(events.into_iter().map(Ok));
        if matches!(self.reply, Reply::Pending) {
            Ok(ProviderStream::new(
                metadata,
                stream.chain(futures::stream::pending()),
                (),
            ))
        } else {
            Ok(ProviderStream::new(metadata, stream, ()))
        }
    }
}

async fn fixture(reply: Reply) -> (axum::Router, Arc<NativeProvider>, Arc<Ports>) {
    fixture_with_middleware(reply, None).await
}

async fn fixture_with_middleware(
    reply: Reply,
    middleware: Option<Arc<super::middleware::RequestMiddleware>>,
) -> (axum::Router, Arc<NativeProvider>, Arc<Ports>) {
    let provider = Arc::new(NativeProvider {
        captures: Mutex::default(),
        reply,
    });
    let ports = Arc::new(Ports::default());
    let execution = Arc::new(DefaultExecutionService::new(
        RuntimeSnapshotHandle::new(snapshot("sk_native_test", "deepseek")),
        ports.clone(),
        ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).unwrap(),
        ports.clone(),
        Arc::new(UnusedContinuation),
        Arc::new(IgnoredClientApiKeyUsage),
        Arc::new(crate::support::RecordingDiagnostics::default()),
    ));
    let execution: Arc<dyn ExecutionService> = match middleware {
        Some(middleware) => Arc::new(MiddlewareExecution {
            inner: execution,
            middleware,
        }),
        None => execution,
    };
    (api_router(execution).await, provider, ports)
}

fn request(path: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .header(
            if path == "/v1/messages" {
                "x-api-key"
            } else {
                "authorization"
            },
            if path == "/v1/messages" {
                "sk_native_test"
            } else {
                "Bearer sk_native_test"
            },
        )
        .header("anthropic-version", "2023-06-01")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn native_protocols_preserve_payload_and_settle_usage_for_stream_and_json() {
    for (path, protocol) in [
        ("/v1/chat/completions", "openai-chat"),
        ("/v1/messages", "anthropic"),
    ] {
        for stream in [false, true] {
            let (app, provider, ports) = fixture(Reply::Normal).await;
            let payload = json!({"model":"model-a","messages":[{"role":"user","content":"hello"}],"stream":stream,"max_tokens":20,"future":{"keep":true}});
            let response = app.oneshot(request(path, payload.clone())).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let text = std::str::from_utf8(&body).unwrap();
            assert!(text.contains("9007199254740993"));
            if stream {
                assert!(text.ends_with(if protocol == "anthropic" {
                    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
                } else {
                    "data: [DONE]\n\n"
                }));
            } else {
                assert_eq!(
                    text,
                    "{ \"id\": \"native_response\", \"future\":9007199254740993, \"content\":\"hello\" }"
                );
            }
            let captures = provider.captures.lock().unwrap();
            assert_eq!(captures.len(), 1);
            assert_eq!(captures[0].protocol(), protocol);
            let Operation::Generate(generate) = &captures[0] else {
                unreachable!()
            };
            assert_eq!(
                generate.protocol_payload().body(),
                payload.as_object().unwrap()
            );
            assert!(
                !generate
                    .protocol_payload()
                    .context()
                    .contains_key("x-api-key")
            );
            assert_eq!(ports.active.load(Ordering::SeqCst), 0);
            let finals = ports.finalizations.lock().unwrap();
            assert_eq!(finals.len(), 1);
            assert_eq!(finals[0].usage.input_tokens, Some(12));
            assert_eq!(finals[0].usage.output_tokens, Some(3));
        }
    }
}

#[tokio::test]
async fn native_auth_and_malformed_requests_never_execute_provider() {
    for path in ["/v1/chat/completions", "/v1/messages"] {
        let (app, provider, _) = fixture(Reply::Normal).await;
        let response = app
            .clone()
            .oneshot(Request::post(path).body(Body::from("{}")).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        if path == "/v1/messages" {
            let value: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(value["type"], "error");
        }
        for body in [
            json!({"messages":[]}),
            json!({"model":"model-a","messages":"invalid"}),
            json!({"model":"model-a","messages":[],"stream":1}),
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(request(path, body))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert!(provider.captures.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn native_missing_terminal_fails_delivery_and_wrong_protocol_fails_before_commit() {
    for path in ["/v1/chat/completions", "/v1/messages"] {
        let (app, _, _) = fixture(Reply::MissingTerminal).await;
        let response = app
            .oneshot(request(
                path,
                json!({"model":"model-a","messages":[],"stream":true}),
            ))
            .await
            .unwrap();
        assert!(to_bytes(response.into_body(), usize::MAX).await.is_err());
        let (app, _, _) = fixture(Reply::WrongProtocol).await;
        let response = app
            .oneshot(request(
                path,
                json!({"model":"model-a","messages":[],"stream":false}),
            ))
            .await
            .unwrap();
        assert!(response.status().is_server_error());
    }
}

#[tokio::test]
async fn native_terminal_only_stream_is_accepted_and_finalized() {
    for path in ["/v1/chat/completions", "/v1/messages"] {
        let (app, _, ports) = fixture(Reply::TerminalOnly).await;
        let response = app
            .oneshot(request(
                path,
                json!({"model":"model-a","messages":[],"stream":true}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(ports.finalizations.lock().unwrap().len(), 1);
        assert!(
            !to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(ports.finalizations.lock().unwrap().len(), 1);
        assert_eq!(ports.active.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn dropping_native_stream_releases_admission_and_finalizes_once() {
    for path in ["/v1/chat/completions", "/v1/messages"] {
        let (app, _, ports) = fixture(Reply::Pending).await;
        let response = app
            .oneshot(request(
                path,
                json!({"model":"model-a","messages":[],"stream":true}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(ports.active.load(Ordering::SeqCst), 1);
        drop(response);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while ports.active.load(Ordering::SeqCst) != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(ports.finalizations.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn native_upstream_validation_preserves_status_and_error_details() {
    for path in ["/v1/chat/completions", "/v1/messages"] {
        for stream in [false, true] {
            let (app, _, ports) = fixture(Reply::UpstreamError).await;
            let response = app
                .oneshot(request(
                    path,
                    json!({"model":"model-a","messages":[],"stream":stream}),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let value: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(value["error"]["message"], "upstream validation");
            assert_eq!(value["future"], json!(9007199254740993u64));
            if path == "/v1/messages" {
                assert_eq!(value["type"], "error");
            }
            assert_eq!(ports.active.load(Ordering::SeqCst), 0);
        }
    }
}

struct MiddlewareExecution {
    inner: Arc<DefaultExecutionService>,
    middleware: Arc<super::middleware::RequestMiddleware>,
}
impl ExecutionService for MiddlewareExecution {
    fn authenticate(
        &self,
        plaintext: &str,
    ) -> Result<AuthenticatedClient, gateway_core::engine::execution::ClientAuthenticationError>
    {
        self.inner.authenticate(plaintext)
    }
    fn public_models(
        &self,
        client: &AuthenticatedClient,
    ) -> Vec<gateway_core::routing::PublicModelId> {
        self.inner.public_models(client)
    }
    fn contains_public_model(
        &self,
        client: &AuthenticatedClient,
        model: &gateway_core::routing::PublicModelId,
    ) -> bool {
        self.inner.contains_public_model(client, model)
    }
    fn prepare_execution(
        &self,
        client: AuthenticatedClient,
    ) -> BoxFuture<
        '_,
        Result<
            gateway_core::engine::execution::PreparedRootExecution,
            gateway_core::error::GatewayError,
        >,
    > {
        self.inner.prepare_execution(client)
    }
    fn middleware_plan(
        &self,
        _: &gateway_core::engine::execution::PreparedRootExecution,
    ) -> Option<gateway_core::engine::middleware::FrozenMiddlewarePlan> {
        Some(self.middleware.frozen())
    }
    fn start_prepared(
        &self,
        prepared: gateway_core::engine::execution::PreparedRootExecution,
        request: gateway_core::engine::execution::PreparedExecutionRequest,
    ) -> BoxFuture<
        '_,
        Result<
            gateway_core::engine::execution::StartedExecution,
            gateway_core::error::GatewayError,
        >,
    > {
        self.inner.start_prepared(prepared, request)
    }
    fn start(
        &self,
        request: gateway_core::engine::execution::StartExecution,
    ) -> BoxFuture<
        '_,
        Result<
            gateway_core::engine::execution::StartedExecution,
            gateway_core::error::GatewayError,
        >,
    > {
        self.inner.start(request)
    }
    fn start_provider_endpoint(
        &self,
        request: gateway_core::engine::execution::StartProviderExecution,
    ) -> BoxFuture<
        '_,
        Result<
            gateway_core::engine::execution::StartedExecution,
            gateway_core::error::GatewayError,
        >,
    > {
        self.inner.start_provider_endpoint(request)
    }
}

#[tokio::test]
async fn middleware_cannot_erase_native_terminal_while_retaining_terminal_flag() {
    use super::middleware::{RequestMiddleware, ResponseFrameAction};
    for path in ["/v1/chat/completions", "/v1/messages"] {
        for single_frame in [true, false] {
            let mut actions = Vec::new();
            if !single_frame {
                actions.push(ResponseFrameAction::Continue);
            }
            actions.push(ResponseFrameAction::Replace(bytes::Bytes::from_static(
                br#"data: {"replacement":true}

"#,
            )));
            let middleware = Arc::new(
                RequestMiddleware {
                    expected_operation: Some(gateway_core::operation::OperationKind::Generate),
                    ..Default::default()
                }
                .with_response_actions(actions),
            );
            let (app, _, ports) = fixture_with_middleware(
                if single_frame {
                    Reply::TerminalOnly
                } else {
                    Reply::Normal
                },
                Some(middleware),
            )
            .await;
            let response = app
                .oneshot(request(
                    path,
                    json!({"model":"model-a","messages":[],"stream":true}),
                ))
                .await
                .unwrap();
            if single_frame {
                assert!(response.status().is_server_error());
            } else {
                assert_eq!(response.status(), StatusCode::OK);
                assert!(to_bytes(response.into_body(), usize::MAX).await.is_err());
            }
            assert_eq!(ports.active.load(Ordering::SeqCst), 0);
        }
    }
}
