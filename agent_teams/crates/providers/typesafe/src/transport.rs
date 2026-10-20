//! 单次原生 JSON 转发，发送冷启动与取消边界由 Core deadline 约束

use crate::{provider::not_sent, schema, selection::Selected};
use bytes::Bytes;
use futures::StreamExt;
use gateway_core::{
    account::OutboundProxy,
    engine::{AttemptContext, middleware::MiddlewareHeader, provider::EventStream},
    error::{ClientVisibleUpstreamResponse, ProviderError, ProviderErrorKind},
    event::{
        FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent, ProviderResponseHeader,
        ProviderResponseObservation, ResponseMeta,
    },
    operation::StructuredInferenceRequest,
    provider_ports::ProviderStorePorts,
    routing::UpstreamModelId,
    upstream::{OpaqueUpstreamValue, UpstreamSendState, UpstreamTransport},
};
use std::{sync::Arc, time::Duration};

const MAX_RESPONSE: usize = 4 * 1024 * 1024;
pub(crate) fn client(proxy: Option<&OutboundProxy>) -> Result<reqwest::Client, ProviderError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .no_proxy();
    if let Some(proxy) = proxy {
        builder = builder.proxy(
            reqwest::Proxy::all(proxy.expose_url())
                .map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?,
        );
    }
    builder
        .build()
        .map_err(|_| not_sent(ProviderErrorKind::Transport))
}
fn sent(kind: ProviderErrorKind) -> ProviderError {
    ProviderError::new(kind, UpstreamSendState::Sent).with_retry_prohibited()
}
pub(crate) async fn bounded_body(
    response: reqwest::Response,
    limit: usize,
) -> Result<Bytes, ProviderError> {
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| sent(ProviderErrorKind::Transport))?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(sent(ProviderErrorKind::Protocol));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(bytes))
}

fn error_response(status: u16, retry: Option<Bytes>) -> ProviderError {
    let kind = match status {
        400 | 404 | 415 | 422 => ProviderErrorKind::InvalidRequest,
        401 => ProviderErrorKind::Unauthorized,
        402 => ProviderErrorKind::QuotaExhausted,
        403 => ProviderErrorKind::PermissionDenied,
        413 => ProviderErrorKind::MessageTooBig,
        429 => ProviderErrorKind::RateLimited,
        500..=599 => ProviderErrorKind::Unavailable,
        _ => ProviderErrorKind::Protocol,
    };
    // 错误正文可能回显用户输入或多层转义凭据，客户端仅接收固定安全错误和 Retry-After
    let body = Bytes::from_static(br#"{"error":{"type":"upstream_error","message":"TypeSafe upstream rejected the request"}}"#);
    let mut error = ProviderError::new(kind, UpstreamSendState::Sent).with_status(status);
    let mut headers = Vec::new();
    if let Some(retry) = retry {
        if let Ok(raw) = std::str::from_utf8(&retry)
            && let Some(seconds) = gateway_protocol::openai::parse_retry_after_seconds(raw)
        {
            error = error.with_retry_after(Duration::from_secs(seconds));
        }
        headers.push(ProviderResponseHeader::new("retry-after", retry));
    }
    error = error.with_client_visible_upstream_response(
        ClientVisibleUpstreamResponse::new(status, Some(b"application/json".to_vec()), body)
            .with_headers(headers),
    );
    // TypeSafe 限流按组织计，不能靠轮换同组织 Key 绕过；其他调用方错误也不自动重放
    if !matches!(status, 401 | 402 | 403 | 500..=599) {
        error = error.with_retry_prohibited();
    }
    error
}

pub(crate) fn stream(
    client: reqwest::Client,
    inference: StructuredInferenceRequest,
    headers: Vec<MiddlewareHeader>,
    model: UpstreamModelId,
    selected: Arc<Selected>,
    context: AttemptContext,
    ports: ProviderStorePorts,
) -> Result<EventStream, ProviderError> {
    let protocol = inference.payload().protocol().to_owned();
    let shape = schema::validate_request(inference.payload())?;
    if shape.model != model.as_str() {
        return Err(not_sent(ProviderErrorKind::InvalidRequest));
    }
    let encoded = inference.payload().body().clone();
    let endpoint = selected
        .credential
        .endpoint(&protocol)
        .map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?;
    let images_enabled = schema::images_enabled(inference.payload());
    let mut request = client
        .post(endpoint)
        .header("content-type", "application/json")
        .header("accept", "application/json");
    let connection_options = headers
        .iter()
        .filter(|h| h.name().eq_ignore_ascii_case("connection"))
        .flat_map(|h| {
            String::from_utf8_lossy(h.value())
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for header in headers {
        let name = header.name().to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "typesafe-danger-zone-images" | "typesafe-danger-zone-long-context"
        ) {
            if header.value().as_ref() != b"false"
                && !(name == "typesafe-danger-zone-images"
                    && images_enabled
                    && header.value().as_ref() == b"true")
            {
                return Err(not_sent(ProviderErrorKind::Unsupported));
            }
            continue;
        }
        if connection_options.contains(&name)
            || gateway_protocol::openai::is_transport_managed_request_header(&name)
            || matches!(
                name.as_str(),
                "authorization" | "x-api-key" | "cookie" | "set-cookie" | "content-type" | "accept"
            )
        {
            continue;
        }
        request = request.header(
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?,
            reqwest::header::HeaderValue::from_bytes(header.value())
                .map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?,
        );
    }
    // 最终图片开关由已校验的客户端上下文拥有，中间件不能覆盖或悄悄启用
    if images_enabled {
        request = request.header("typesafe-danger-zone-images", "true");
    }
    let request = request
        .bearer_auth(&selected.credential.api_key)
        .body(encoded)
        .build()
        .map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?;
    Ok(Box::pin(async_stream::try_stream! {
        let cancellation = context.cancellation().clone();
        if cancellation.is_cancelled() { Err(not_sent(ProviderErrorKind::Cancelled))?; }
        if context.deadline().is_elapsed() { Err(not_sent(ProviderErrorKind::Timeout))?; }
        let price = crate::pricing::resolve(model.as_str(), context.pricing().get("typesafe").and_then(|models| models.get(model.as_str())));
        let response = tokio::select! { biased;
            _ = cancellation.cancelled() => Err(ProviderError::new(ProviderErrorKind::Cancelled, UpstreamSendState::Ambiguous)),
            _ = context.deadline().wait() => Err(ProviderError::new(ProviderErrorKind::Timeout, UpstreamSendState::Ambiguous)),
            result = client.execute(request) => result.map_err(|error| ProviderError::new(if error.is_timeout() { ProviderErrorKind::Timeout } else { ProviderErrorKind::Transport }, if error.is_connect() { UpstreamSendState::NotSent } else { UpstreamSendState::Ambiguous }))
        }?;
        let status = response.status().as_u16();
        let response_id = response.headers().get("x-typesafe-request-id").or_else(|| response.headers().get("x-request-id"))
            .and_then(|v|v.to_str().ok()).filter(|s| !s.is_empty() && s.len() <= 255
                && s.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_-.:".contains(&byte))
                && !s.contains(&selected.credential.api_key)).unwrap_or("").to_owned();
        if !response.status().is_success() {
            let retry = response.headers().get("retry-after").filter(|v| v.to_str().is_ok_and(|s| !s.contains(&selected.credential.api_key))).map(|v| Bytes::copy_from_slice(v.as_bytes()));
            // 不消费或存储上游错误正文，避免认证信息经错误回显泄漏
            drop(response);
            if status == 401 && selected.account.enabled() {
                let change = gateway_core::account::AccountStateChange {
                    account_id: selected.account.id().clone(), expected_revision: selected.account.revision(),
                    credential_state: gateway_core::account::CredentialState::Invalid,
                    observed_at: std::time::SystemTime::now(), error_reason: Some(gateway_core::account::AccountErrorReason::CredentialInvalid),
                    message: Some("TypeSafe API key rejected".to_owned()),
                };
                let accounts = ports.accounts();
                tokio::select! { biased;
                    _ = cancellation.cancelled() => Err(sent(ProviderErrorKind::Cancelled)),
                    _ = context.deadline().wait() => Err(sent(ProviderErrorKind::Timeout)),
                    result = accounts.apply_state_change(change) => result.map(|_| ()).map_err(|_| sent(ProviderErrorKind::ProviderInfrastructureUnavailable)),
                }?;
            }
            let mut error = error_response(status, retry);
            if !response_id.is_empty() { error = error.with_upstream_request_id(OpaqueUpstreamValue::new(response_id)); }
            Err(error)?;
            return;
        }
        let bytes = tokio::select! { biased;
            _ = cancellation.cancelled() => Err(sent(ProviderErrorKind::Cancelled)),
            _ = context.deadline().wait() => Err(sent(ProviderErrorKind::Timeout)),
            result = bounded_body(response, MAX_RESPONSE) => result,
        }?;
        let (actual_model, usage) = schema::validate_response(&bytes, &shape)?;
        // 成功结果也可能回显账号 Key，拒绝该结果而不改写用户评估结果
        if contains_secret(&bytes, &selected.credential.api_key) { Err(sent(ProviderErrorKind::Protocol))?; }
        let meta = ResponseMeta::new(&response_id, &actual_model).with_finish_reason(FinishReason::Stop);
        let mut facts = vec![GatewayEvent::Started(meta.clone()), GatewayEvent::Usage(usage.clone())];
        if let Some(price) = price && let Some(cost) = crate::pricing::calculate(&usage, &price) { facts.push(GatewayEvent::CalculatedCost(cost)); }
        facts.push(GatewayEvent::Completed(meta));
        let mut event = ProviderEvent::canonical_with_wire(facts, ProtocolWireEvent::raw_json(&protocol, bytes).map_err(|_|sent(ProviderErrorKind::Protocol))?);
        let mut observation = ProviderResponseObservation::new(UpstreamTransport::new("http_json").map_err(|_| sent(ProviderErrorKind::Protocol))?).with_status_code(status).with_upstream_response_model_if_valid(&actual_model);
        if !response_id.is_empty() { observation = observation.with_request_id(OpaqueUpstreamValue::new(response_id)); }
        event.attach_observation(observation);
        yield event;
    }))
}

pub(crate) fn contains_secret(bytes: &[u8], secret: &str) -> bool {
    fn nested(value: &serde_json::Value, secret: &str, depth: usize) -> bool {
        match value {
            serde_json::Value::String(s) => {
                s.contains(secret)
                    || (depth > 0
                        && serde_json::from_str::<serde_json::Value>(s)
                            .is_ok_and(|v| nested(&v, secret, depth - 1)))
            }
            serde_json::Value::Object(o) => o
                .iter()
                .any(|(key, value)| key.contains(secret) || nested(value, secret, depth)),
            serde_json::Value::Array(a) => a.iter().any(|v| nested(v, secret, depth)),
            _ => false,
        }
    }
    bytes
        .windows(secret.len())
        .any(|window| window == secret.as_bytes())
        || serde_json::from_slice(bytes).is_ok_and(|value| nested(&value, secret, 4))
}
