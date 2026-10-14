//! Chat 与 Messages 原生入口，复用请求洋葱链和 Core 的提交、取消及结算边界

use std::{collections::VecDeque, net::SocketAddr};

use axum::{
    body::Bytes,
    extract::{Extension, State, connect_info::ConnectInfo},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse as _, Response},
};
use futures::future::BoxFuture;
use gateway_core::{
    engine::{
        execution::{
            ClientTransport, ExecutionRequestMetadata, ExecutionSession, PreparedExecutionRequest,
            StartedExecution,
        },
        middleware::{
            MiddlewareBody, MiddlewareError, MiddlewareFrame, MiddlewareFraming, MiddlewareHeader,
            MiddlewareResponse,
        },
    },
    error::{GatewayError, GatewayErrorKind},
    event::ProviderEvent,
    lifecycle::ConnectionGuard,
    operation::{GenerateRequest, Operation, OperationKind, ProtocolPayload},
    routing::PublicModelId,
};
use serde_json::{Value, json};

use super::{
    auth::{authenticate_client, client_access_error_response},
    error::gateway_error_response,
    middleware::{
        ExpectedBody, PendingExecution, RequestInput, error_response, finalize_session,
        into_protocol_http_response, invoke_request, pending_execution_response, request_parts,
    },
    responses::{OpenAiRequestHeaders, decompress_request_body, request_client_context},
};
use crate::{ApiState, middleware::headers::encode_headers};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Chat,
    Messages,
}

impl Format {
    fn protocol(self) -> &'static str {
        match self {
            Self::Chat => "openai-chat",
            Self::Messages => "anthropic",
        }
    }
    fn endpoint(self) -> &'static str {
        match self {
            Self::Chat => "/v1/chat/completions",
            Self::Messages => "/v1/messages",
        }
    }
}

pub(super) async fn chat(
    State(state): State<ApiState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    generate(state, peer, headers, body, Format::Chat).await
}

pub(super) async fn messages(
    State(state): State<ApiState>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let response = generate(state, peer, headers, body, Format::Messages).await;
    messages_error_envelope(response).await
}

async fn generate(
    state: ApiState,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    mut headers: HeaderMap,
    body: Bytes,
    format: Format,
) -> Response {
    if format == Format::Messages && normalize_messages_auth(&mut headers).is_err() {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"error":{"type":"authentication_error","message":"Invalid API key headers"}}))).into_response();
    }
    let service = state.openai().clone();
    let client = match authenticate_client(&service, &headers).await {
        Ok(client) => client,
        Err(error) => return client_access_error_response(error),
    };
    let body = match decompress_request_body(
        &body,
        &headers,
        client.snapshot().responses_max_decompressed_body_bytes(),
    ) {
        Ok(body) => Bytes::copy_from_slice(&body),
        Err(error) => {
            return super::error::protocol_error_response(
                StatusCode::BAD_REQUEST,
                error.protocol_body(),
            );
        }
    };
    headers.remove(header::CONTENT_ENCODING);
    headers.remove(header::CONTENT_LENGTH);
    let (_, model, streaming) = match decode(&body, &headers, format) {
        Ok(decoded) => decoded,
        Err(error) => return gateway_error_response(&error),
    };
    let transport = if streaming {
        ClientTransport::HttpSse
    } else {
        ClientTransport::HttpJson
    };
    let (client_ip, user_agent) = request_client_context(
        &headers,
        peer.map(|Extension(ConnectInfo(address))| address),
    );
    let execution = service.execution();
    let prepared = match execution.prepare_execution(client).await {
        Ok(prepared) => prepared,
        Err(error) => return gateway_error_response(&error),
    };
    let request_id = prepared.request_id().clone();
    let terminal_execution = execution.clone();
    let result = invoke_request(
        execution,
        prepared,
        RequestInput {
            endpoint: format.endpoint().to_owned(),
            protocol: format.protocol().to_owned(),
            operation: Some(OperationKind::Generate),
            transport,
            model_hint: Some(model),
            headers: encode_headers(&headers),
            body,
        },
        Box::new(move |prepared, request| {
            Box::pin(async move {
                let (protocol, headers, body) = request_parts(request.clone())?;
                if protocol != format.protocol() {
                    return Err(MiddlewareError::Rejected);
                }
                let (operation, model, stream) = decode(&body, &headers, format)?;
                if stream != streaming {
                    return Err(MiddlewareError::Rejected);
                }
                let operation = request.apply_capabilities(operation)?;
                let connection = if streaming {
                    Some(
                        service
                            .try_register_connection()
                            .map_err(|_| MiddlewareError::Fault)?,
                    )
                } else {
                    None
                };
                let started = terminal_execution
                    .start_prepared(
                        prepared,
                        PreparedExecutionRequest {
                            public_model: PublicModelId::from_client_wire(model)
                                .map_err(|_| invalid_request("Invalid model"))?,
                            operation,
                            metadata: ExecutionRequestMetadata {
                                protocol: protocol.clone(),
                                endpoint: format.endpoint().to_owned(),
                                transport,
                                stream,
                                client_ip,
                                user_agent,
                                previous_response_id: None,
                            },
                        },
                    )
                    .await?;
                started
                    .session
                    .trace()
                    .capture("client.request.body", &body);
                deliver(started, format, streaming, connection).await
            })
        }),
    )
    .await;
    let response = match result {
        Ok(response) => {
            let response = if streaming && (200..300).contains(&response.status_code()) {
                validated_stream_response(response, format)
            } else {
                response
            };
            into_protocol_http_response(
                response,
                if streaming {
                    ExpectedBody::Sse
                } else {
                    ExpectedBody::SingleJson
                },
                format.protocol(),
            )
            .await
        }
        Err(error) => error_response(error),
    };
    super::with_model_request_id(response, &request_id)
}

fn normalize_messages_auth(headers: &mut HeaderMap) -> Result<(), ()> {
    if headers.get_all("x-api-key").iter().count() > 1
        || headers.get_all(header::AUTHORIZATION).iter().count() > 1
    {
        return Err(());
    }
    if let Some(key) = headers.get("x-api-key") {
        let key = key.to_str().map_err(|_| ())?;
        if key.is_empty() {
            return Err(());
        }
        let bearer = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| ())?;
        if headers
            .get(header::AUTHORIZATION)
            .is_some_and(|value| value != bearer)
        {
            return Err(());
        }
        headers.insert(header::AUTHORIZATION, bearer);
    }
    Ok(())
}

fn invalid_request(message: &'static str) -> GatewayError {
    GatewayError::new(GatewayErrorKind::InvalidRequest, message)
}

fn decode(
    body: &[u8],
    headers: &HeaderMap,
    format: Format,
) -> Result<(Operation, String, bool), GatewayError> {
    let object = serde_json::from_slice::<serde_json::Map<String, Value>>(body)
        .map_err(|_| invalid_request("Expected a JSON object"))?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|model| !model.trim().is_empty())
        .ok_or_else(|| invalid_request("model is required"))?
        .to_owned();
    if !object.get("messages").is_some_and(Value::is_array) {
        return Err(invalid_request("messages must be an array"));
    }
    let stream = match object.get("stream") {
        None => false,
        Some(Value::Bool(value)) => *value,
        _ => return Err(invalid_request("stream must be boolean")),
    };
    let mut context = OpenAiRequestHeaders::from_headers(headers).session_context();
    // 认证材料不会进入 Provider 上下文，只保留 Messages 的业务版本头
    for name in ["anthropic-version", "anthropic-beta"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            context.insert(name.to_owned(), Value::String(value.to_owned()));
        }
    }
    let payload = ProtocolPayload::json_object(format.protocol(), object)
        .map_err(|_| invalid_request("Invalid protocol"))?
        .with_context(context);
    Ok((
        Operation::Generate(GenerateRequest::from_protocol_payload(payload)),
        model,
        stream,
    ))
}

async fn deliver(
    started: StartedExecution,
    format: Format,
    streaming: bool,
    connection: Option<Box<dyn ConnectionGuard>>,
) -> Result<MiddlewareResponse, MiddlewareError> {
    if !streaming {
        let mut pending = PendingExecution::new(started.session);
        let session = pending.session_mut().ok_or(MiddlewareError::InvalidState)?;
        let events = match session.collect_uncommitted().await {
            Ok(events) => events,
            Err(error) => return native_engine_error_response(pending, format, &error).await,
        };
        let transformed = events.iter().any(ProviderEvent::middleware_transformed);
        let mut document = None;
        for event in events {
            if let Some(wire) = event.wire_event() {
                if wire.protocol() != format.protocol() {
                    return Err(MiddlewareError::InvalidState);
                }
                if let Some(raw) = wire.raw_json_body()
                    && document.replace(raw.clone()).is_some()
                {
                    return Err(MiddlewareError::InvalidState);
                }
            }
        }
        let bytes = document.ok_or(MiddlewareError::InvalidState)?;
        let headers = response_headers(session.response_headers(), false);
        return Ok(pending_execution_response(
            format.protocol().to_owned(),
            200,
            headers,
            MiddlewareFrame::new(bytes, MiddlewareFraming::JsonDocument, true)
                .with_transformed(transformed),
            pending,
        ));
    }
    let mut body = NativeStream {
        session: Some(started.session),
        format,
        pending: VecDeque::new(),
        committed: false,
        batch_pending: false,
        terminal_seen: false,
        finished: false,
        _connection: connection,
    };
    if let Err(error) = body.fill().await {
        return match error {
            MiddlewareError::Engine(error) => {
                let session = body.session.take().ok_or(MiddlewareError::InvalidState)?;
                native_engine_error_response(PendingExecution::new(session), format, &error).await
            }
            other => Err(other),
        };
    }
    let headers = response_headers(
        body.session
            .as_ref()
            .ok_or(MiddlewareError::InvalidState)?
            .response_headers(),
        true,
    );
    Ok(MiddlewareResponse::new(
        format.protocol().to_owned(),
        200,
        headers,
        Box::new(body),
    ))
}

fn validated_stream_response(response: MiddlewareResponse, format: Format) -> MiddlewareResponse {
    let (protocol, status, headers, inner, envelope) = response.into_parts();
    let mut response = MiddlewareResponse::new(
        protocol,
        status,
        headers,
        Box::new(ValidatedNativeBody { inner, format }),
    );
    if let Some(envelope) = envelope {
        response = response.with_envelope(envelope);
    }
    response
}

/// 返回链改写后的帧仍须包含对应协议的真实终态，不能仅信任中间件的 terminal 标志
struct ValidatedNativeBody {
    inner: Box<dyn MiddlewareBody>,
    format: Format,
}

impl MiddlewareBody for ValidatedNativeBody {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            let frame = self.inner.next_frame().await?;
            if let Some(frame) = &frame
                && frame.terminal() != terminal_frame(self.format, frame.bytes())
            {
                return Err(MiddlewareError::InvalidState);
            }
            Ok(frame)
        })
    }

    fn commit_downstream(
        &mut self,
        status: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        self.inner.commit_downstream(status)
    }

    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        self.inner.record_client_status(status)
    }

    fn is_finalized(&self) -> bool {
        self.inner.is_finalized()
    }

    fn close(self: Box<Self>) -> BoxFuture<'static, ()> {
        self.inner.close()
    }
}

async fn native_engine_error_response(
    execution: PendingExecution,
    format: Format,
    error: &gateway_core::engine::EngineError,
) -> Result<MiddlewareResponse, MiddlewareError> {
    // 在洋葱链完成后才释放失败执行，保留上游状态码、正文与允许透传的错误头
    let response = super::error::engine_error_response(error);
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX)
        .await
        .map_err(|_| MiddlewareError::Fault)?;
    Ok(pending_execution_response(
        format.protocol().to_owned(),
        parts.status.as_u16(),
        encode_headers(&parts.headers),
        MiddlewareFrame::new(bytes, MiddlewareFraming::RawBytes, true),
        execution,
    ))
}

fn response_headers(
    headers: &[gateway_core::event::ProviderResponseHeader],
    streaming: bool,
) -> Vec<MiddlewareHeader> {
    let connection = super::responses::response_connection_options(headers);
    let mut result = vec![MiddlewareHeader::new(
        "content-type",
        Bytes::from_static(if streaming {
            b"text/event-stream"
        } else {
            b"application/json"
        }),
    )];
    for header in headers {
        if !header.name().eq_ignore_ascii_case("content-type")
            && super::responses::response_header_is_forwardable(header.name(), &connection)
        {
            result.push(MiddlewareHeader::new(
                header.name(),
                Bytes::copy_from_slice(header.value()),
            ));
        }
    }
    result
}

struct NativeStream {
    session: Option<Box<dyn ExecutionSession>>,
    format: Format,
    pending: VecDeque<MiddlewareFrame>,
    committed: bool,
    batch_pending: bool,
    terminal_seen: bool,
    finished: bool,
    _connection: Option<Box<dyn ConnectionGuard>>,
}

impl NativeStream {
    async fn fill(&mut self) -> Result<(), MiddlewareError> {
        while self.pending.is_empty() && !self.finished {
            let session = self.session.as_mut().ok_or(MiddlewareError::InvalidState)?;
            if self.batch_pending && !self.committed {
                session.discard_pending_delivery()?;
            }
            self.batch_pending = false;
            let Some(batch) = session.next_event().await? else {
                if !session.is_finalized() || !self.terminal_seen {
                    return Err(MiddlewareError::InvalidState);
                }
                self.finished = true;
                break;
            };
            if self.terminal_seen {
                return Err(MiddlewareError::InvalidState);
            }
            for event in batch.into_provider_events() {
                let Some(wire) = event.wire_event() else {
                    continue;
                };
                if self.terminal_seen {
                    return Err(MiddlewareError::InvalidState);
                }
                if wire.protocol() != self.format.protocol() {
                    return Err(MiddlewareError::InvalidState);
                }
                let bytes = if let Some(raw) = wire.raw_sse_frame() {
                    raw.clone()
                } else {
                    Bytes::from(
                        gateway_protocol::openai::sse::encode_sse_event_with_metadata(
                            wire.event_type().unwrap_or_default(),
                            &wire.data().to_string(),
                            wire.sse_id(),
                            wire.sse_retry(),
                        ),
                    )
                };
                let terminal = terminal_frame(self.format, &bytes);
                self.terminal_seen |= terminal;
                self.pending.push_back(
                    MiddlewareFrame::new(bytes, MiddlewareFraming::SseEvent, terminal)
                        .with_transformed(event.middleware_transformed()),
                );
            }
            self.batch_pending = !self.committed;
            // 已交付的流必须先完成 Core 结算，再发送原生成功终态
            if self.terminal_seen && self.committed {
                if session.next_event().await?.is_some() || !session.is_finalized() {
                    return Err(MiddlewareError::InvalidState);
                }
                self.finished = true;
            }
        }
        Ok(())
    }
}

fn terminal_frame(format: Format, bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.lines().any(|line| match format {
        Format::Chat => line
            .strip_prefix("data:")
            .is_some_and(|value| value.trim() == "[DONE]"),
        Format::Messages => {
            line.strip_prefix("event:")
                .is_some_and(|value| matches!(value.trim(), "message_stop" | "error"))
                || line
                    .strip_prefix("data:")
                    .and_then(|value| serde_json::from_str::<Value>(value.trim()).ok())
                    .is_some_and(|data| {
                        matches!(
                            data.get("type").and_then(Value::as_str),
                            Some("message_stop" | "error")
                        )
                    })
        }
    })
}

impl MiddlewareBody for NativeStream {
    fn next_frame(&mut self) -> BoxFuture<'_, Result<Option<MiddlewareFrame>, MiddlewareError>> {
        Box::pin(async move {
            self.fill().await?;
            Ok(self.pending.pop_front())
        })
    }
    fn commit_downstream(
        &mut self,
        status: Option<u16>,
    ) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            if !self.committed {
                self.session
                    .as_mut()
                    .ok_or(MiddlewareError::InvalidState)?
                    .commit_downstream(status)
                    .await?;
                self.committed = true;
                self.batch_pending = false;
                if self.terminal_seen {
                    let session = self.session.as_mut().ok_or(MiddlewareError::InvalidState)?;
                    if session.next_event().await?.is_some() || !session.is_finalized() {
                        return Err(MiddlewareError::InvalidState);
                    }
                    self.finished = true;
                }
            }
            Ok(())
        })
    }
    fn record_client_status(&mut self, status: u16) -> BoxFuture<'_, Result<(), MiddlewareError>> {
        Box::pin(async move {
            if let Some(session) = self.session.as_mut() {
                session.record_client_status(status).await?;
            }
            Ok(())
        })
    }
    fn is_finalized(&self) -> bool {
        self.session
            .as_ref()
            .is_none_or(|session| session.is_finalized())
    }
    fn close(mut self: Box<Self>) -> BoxFuture<'static, ()> {
        let session = self.session.take();
        let connection = self._connection.take();
        Box::pin(async move {
            if let Some(session) = session {
                finalize_session(session).await;
            }
            drop(connection);
        })
    }
}

impl Drop for NativeStream {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            session.cancel();
            let finalize = session.detach_finalize();
            let connection = self._connection.take();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                drop(runtime.spawn(async move {
                    finalize.await;
                    drop(connection);
                }));
            }
        }
    }
}

async fn messages_error_envelope(response: Response) -> Response {
    if response.status().is_success() {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    };
    if let Some(object) = value.as_object_mut() {
        if !object.contains_key("type") {
            object.insert("type".to_owned(), json!("error"));
            if let Some(error) = object.get_mut("error").and_then(Value::as_object_mut) {
                let kind = match parts.status.as_u16() {
                    401 => "authentication_error",
                    403 => "permission_error",
                    404 => "not_found_error",
                    413 => "request_too_large",
                    429 => "rate_limit_error",
                    529 => "overloaded_error",
                    400..=499 => "invalid_request_error",
                    _ => "api_error",
                };
                error.insert("type".to_owned(), json!(kind));
            }
        }
        parts.headers.remove(header::CONTENT_LENGTH);
        parts.headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        return Response::from_parts(parts, axum::body::Body::from(value.to_string()));
    }
    Response::from_parts(parts, axum::body::Body::from(bytes))
}
