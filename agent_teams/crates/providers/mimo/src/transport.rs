//! 原生 HTTP 与 SSE 转发，同帧旁路提取计量和终态，网络发送保持 cold

use crate::{provider::not_sent, selection::Selected};
use base64::Engine;
use bytes::Bytes;
use futures::StreamExt;
use gateway_core::{
    account::OutboundProxy,
    engine::{AttemptContext, middleware::MiddlewareHeader, provider::EventStream},
    error::{ProviderError, ProviderErrorKind},
    event::{
        FinishReason, GatewayEvent, ProtocolWireEvent, ProviderEvent, ResponseMeta, TextDelta,
    },
    metering::Usage,
    operation::GenerateRequest,
    provider_ports::{ProviderCooldown, ProviderStorePorts},
    routing::UpstreamModelId,
    upstream::{OpaqueUpstreamValue, UpstreamSendState, UpstreamTransport},
};
use gateway_protocol::openai::{
    is_transport_managed_request_header,
    sse::{SseEventDecoder, SseFrame},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

pub(crate) fn client(proxy: Option<&OutboundProxy>) -> Result<reqwest::Client, ProviderError> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(600))
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
fn sent(kind: ProviderErrorKind) -> ProviderError {
    ProviderError::new(kind, UpstreamSendState::Sent)
}

fn is_streaming(generate: &GenerateRequest) -> bool {
    generate.protocol_payload().protocol() == "openai"
        || generate
            .protocol_payload()
            .body()
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

pub(crate) fn upstream_transport(
    generate: &GenerateRequest,
) -> Result<UpstreamTransport, ProviderError> {
    UpstreamTransport::new(if is_streaming(generate) {
        "http_sse"
    } else {
        "http_json"
    })
    .map_err(|_| not_sent(ProviderErrorKind::Protocol))
}

pub(crate) fn stream(
    client: reqwest::Client,
    generate: GenerateRequest,
    headers: Vec<MiddlewareHeader>,
    model: UpstreamModelId,
    selected: Arc<Selected>,
    context: AttemptContext,
    ports: ProviderStorePorts,
) -> Result<EventStream, ProviderError> {
    let protocol = generate.protocol_payload().protocol().to_owned();
    let endpoint = selected
        .credential
        .endpoint(&protocol)
        .map_err(|_| not_sent(ProviderErrorKind::Unsupported))?;
    let mut body = generate.protocol_payload().body().clone();
    body.insert("model".to_owned(), json!(model.as_str()));
    if has_unsupported_stateful_fields(&generate) {
        return Err(not_sent(ProviderErrorKind::Unsupported));
    }

    // Responses 出口既有聚合器消费 SSE；保留同协议的事件，不跨协议转换正文
    if protocol == "openai" {
        body.insert("stream".to_owned(), Value::Bool(true));
    }
    let streaming = is_streaming(&generate);
    let upstream_transport = upstream_transport(&generate)?;
    if protocol == "openai-chat" && streaming {
        let options = body
            .entry("stream_options".to_owned())
            .or_insert_with(|| json!({}));
        if !options.is_object() {
            return Err(not_sent(ProviderErrorKind::InvalidRequest));
        }
        options["include_usage"] = Value::Bool(true);
    }
    let encoded =
        serde_json::to_vec(&body).map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?;
    let mut request = client
        .post(endpoint)
        .header("content-type", "application/json")
        .header(
            "accept",
            if streaming {
                "text/event-stream"
            } else {
                "application/json"
            },
        );
    let mut all_headers = Vec::new();
    if let Some(entries) = generate
        .protocol_payload()
        .context()
        .get("opaque_request_headers")
        .and_then(Value::as_array)
    {
        for entry in entries {
            if let Some(pair) = entry.as_array().filter(|v| v.len() == 2)
                && let (Some(name), Some(encoded)) = (pair[0].as_str(), pair[1].as_str())
                && let Ok(value) = base64::engine::general_purpose::STANDARD.decode(encoded)
            {
                all_headers.push(MiddlewareHeader::new(name, Bytes::from(value)));
            }
        }
    }
    let overridden = headers
        .iter()
        .map(|h| h.name().to_ascii_lowercase())
        .collect::<std::collections::BTreeSet<_>>();
    all_headers.retain(|h| !overridden.contains(&h.name().to_ascii_lowercase()));
    all_headers.extend(headers);
    let connection_options = all_headers
        .iter()
        .filter(|h| h.name().eq_ignore_ascii_case("connection"))
        .flat_map(|h| {
            String::from_utf8_lossy(h.value())
                .split(',')
                .map(|v| v.trim().to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .collect::<std::collections::BTreeSet<_>>();
    let has_anthropic_version = all_headers
        .iter()
        .any(|h| h.name().eq_ignore_ascii_case("anthropic-version"));
    for header in all_headers {
        let name = header.name().to_ascii_lowercase();
        if connection_options.contains(&name)
            || is_transport_managed_request_header(&name)
            || matches!(
                name.as_str(),
                "authorization"
                    | "x-api-key"
                    | "api-key"
                    | "cookie"
                    | "set-cookie"
                    | "content-type"
                    | "accept"
            )
        {
            continue;
        }
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?;
        let value = reqwest::header::HeaderValue::from_bytes(header.value())
            .map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?;
        request = request.header(name, value);
    }
    if protocol == "anthropic" && !has_anthropic_version {
        request = request.header("anthropic-version", "2023-06-01");
    }
    request = request
        .bearer_auth(&selected.credential.api_key)
        .body(encoded);
    let request = request
        .build()
        .map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?;
    Ok(Box::pin(async_stream::try_stream! {
        let cancellation=context.cancellation().clone();
        if cancellation.is_cancelled(){Err(not_sent(ProviderErrorKind::Cancelled))?;}
        if context.deadline().is_elapsed(){Err(not_sent(ProviderErrorKind::Timeout))?;}
        // 首次轮询才实际发送，使用请求冻结的单价，不把套餐额度换算为上游余额
        let price = crate::pricing::resolve(
            model.as_str(),
            context
                .pricing()
                .get("mimo")
                .and_then(|models| models.get(model.as_str())),
        );
        let response=tokio::select!{biased;_=cancellation.cancelled()=>Err(ProviderError::new(ProviderErrorKind::Cancelled,UpstreamSendState::Ambiguous)),_=context.deadline().wait()=>Err(ProviderError::new(ProviderErrorKind::Timeout,UpstreamSendState::Ambiguous)),result=client.execute(request)=>result.map_err(|error|ProviderError::new(if error.is_timeout(){ProviderErrorKind::Timeout}else{ProviderErrorKind::Transport},UpstreamSendState::Ambiguous))}?;
        if !response.status().is_success(){
            let status=response.status().as_u16();
            let retry=response.headers().get("retry-after").and_then(|v|v.to_str().ok()).and_then(gateway_protocol::openai::parse_retry_after_seconds);
            let kind=match status{400|404|422=>ProviderErrorKind::InvalidRequest,401=>ProviderErrorKind::Unauthorized,402=>ProviderErrorKind::QuotaExhausted,403=>ProviderErrorKind::PermissionDenied,413=>ProviderErrorKind::MessageTooBig,429=>ProviderErrorKind::RateLimited,500..=599=>ProviderErrorKind::Unavailable,_=>ProviderErrorKind::Protocol};
            // 认证失败标记无效；余额与限流仅短暂冷却，充值后仍可恢复且不制造额度快照
            if matches!(status,401|402|429) && selected.account.enabled(){
                let now=std::time::SystemTime::now();
                match status {
                    401=>{let _=ports.accounts().apply_state_change(gateway_core::account::AccountStateChange {account_id:selected.account.id().clone(),expected_revision:selected.account.revision(),credential_state:gateway_core::account::CredentialState::Invalid,observed_at:now,error_reason:Some(gateway_core::account::AccountErrorReason::CredentialInvalid),message:Some("MiMo API key rejected".to_owned())}).await;},
                    _=>{let _=ports.cooldowns().put_if_later(ProviderCooldown::new(selected.account.id().clone(),selected.account.revision(),now+Duration::from_secs(retry.unwrap_or(60).min(86400)))).await;}
                }
            }
            let content_type=response.headers().get("content-type").map(|v|v.as_bytes().to_vec());

            let connection_options=response.headers().get("connection").and_then(|v|v.to_str().ok()).map(|v|v.split(',').map(|s|s.trim().to_owned()).collect::<Vec<_>>()).unwrap_or_default();
            let headers=response.headers().iter().filter(|(name,_)|gateway_protocol::openai::response_header_is_forwardable(name.as_str(),&connection_options)).map(|(name,value)|gateway_core::event::ProviderResponseHeader::new(name.as_str(),Bytes::copy_from_slice(value.as_bytes()))).collect();
            let bytes=tokio::select!{biased;_=cancellation.cancelled()=>Err(sent(ProviderErrorKind::Cancelled)),_=context.deadline().wait()=>Err(sent(ProviderErrorKind::Timeout)),result=bounded_body(response,1024*1024)=>result}?;
            // 上游偶尔回显认证字段；返回错误前移除本账号密钥，不把原文写入日志
            let bytes=redact_error_body(&bytes, &selected.credential.api_key);
            let code=serde_json::from_slice::<Value>(&bytes).ok().and_then(|value|value.pointer("/error/code").or_else(||value.get("code")).and_then(Value::as_str).map(str::to_owned));
            let mut error=sent(kind).with_status(status).with_client_visible_upstream_response(gateway_core::error::ClientVisibleUpstreamResponse::new(status,content_type,bytes).with_headers(headers));
            if let Some(code)=code{error=error.with_upstream_code(OpaqueUpstreamValue::new(code));}
            if let Some(retry)=retry{error=error.with_retry_after(Duration::from_secs(retry));}
            Err(error)?;
            return;
        }
        let connection_options=response.headers().get("connection").and_then(|v|v.to_str().ok()).map(|v|v.split(',').map(|v|v.trim().to_owned()).collect::<Vec<_>>()).unwrap_or_default();
        let headers=response.headers().iter().filter(|(name,_)|gateway_protocol::openai::response_header_is_forwardable(name.as_str(),&connection_options)).map(|(name,value)|gateway_core::event::ProviderResponseHeader::new(name.as_str(),Bytes::copy_from_slice(value.as_bytes()))).collect();
        let mut observation=Some(gateway_core::event::ProviderResponseObservation::new(upstream_transport).with_status_code(response.status().as_u16()).with_client_headers(headers));
        let standard=body.get("service_tier").is_none_or(|v|v.is_null()||matches!(v.as_str(),Some("default"|"standard"))) && body.get("tools").and_then(Value::as_array).is_none_or(|tools|tools.iter().all(|tool|tool.get("type").is_none_or(|v|matches!(v.as_str(),Some("function"|"custom")))));
        let price=standard.then_some(price).flatten();
        let mut decoder=Observation::new(protocol.clone(),model.as_str().to_owned(),selected.credential.api_key.clone(),price);
        if !streaming{
            let bytes=tokio::select!{biased;_=cancellation.cancelled()=>Err(sent(ProviderErrorKind::Cancelled)),_=context.deadline().wait()=>Err(sent(ProviderErrorKind::Timeout)),result=bounded_body(response,usize::MAX)=>result}?;
            let value:Value=serde_json::from_slice(&bytes).map_err(|_|sent(ProviderErrorKind::Protocol))?;
            let facts=decoder.observe(&value,None,true)?;
            if !decoder.finished {Err(sent(ProviderErrorKind::Protocol))?;}
            let mut event=ProviderEvent::canonical_with_wire(facts,ProtocolWireEvent::raw_json(&protocol,bytes).map_err(|_|sent(ProviderErrorKind::Protocol))?);
            if let Some(observation)=observation.take(){event.attach_observation(observation);}
            yield event;
        }else{
            let mut source=response.bytes_stream();let mut sse=SseEventDecoder::default();let mut utf8_tail=Vec::with_capacity(4);
            loop{
                let chunk=tokio::select!{biased;_=cancellation.cancelled()=>Err(sent(ProviderErrorKind::Cancelled)),_=context.deadline().wait()=>Err(sent(ProviderErrorKind::Timeout)),chunk=source.next()=>Ok(chunk)}?;
                let Some(chunk)=chunk else{break;};
                let chunk=chunk.map_err(|_|sent(ProviderErrorKind::Transport))?;
                validate_utf8_chunk(&mut utf8_tail,&chunk)?;
                for frame in sse.push_frames(&chunk){if cancellation.is_cancelled(){Err(sent(ProviderErrorKind::Cancelled))?;}
                if context.deadline().is_elapsed(){Err(sent(ProviderErrorKind::Timeout))?;}let mut event=decoder.frame(frame)?;if let Some(observation)=observation.take(){event.attach_observation(observation);}yield event; if decoder.finished{break;}}
                if decoder.finished{break;}
            }
            if !decoder.finished && !utf8_tail.is_empty(){Err(sent(ProviderErrorKind::Protocol))?;}
            if !decoder.finished {for frame in sse.finish_frames(){if cancellation.is_cancelled(){Err(sent(ProviderErrorKind::Cancelled))?;}
                if context.deadline().is_elapsed(){Err(sent(ProviderErrorKind::Timeout))?;}let mut event=decoder.frame(frame)?;if let Some(observation)=observation.take(){event.attach_observation(observation);}yield event;}}
            if !decoder.finished{Err(sent(ProviderErrorKind::Protocol))?;}
        }
    }))
}

// HTTP chunk 和超限 opaque 帧可截断中文字符，只保留至多三个未完成 UTF-8 字节
fn validate_utf8_chunk(tail: &mut Vec<u8>, mut chunk: &[u8]) -> Result<(), ProviderError> {
    while !tail.is_empty() && !chunk.is_empty() {
        tail.push(chunk[0]);
        chunk = &chunk[1..];
        match std::str::from_utf8(tail) {
            Ok(_) => tail.clear(),
            Err(error) if error.error_len().is_none() => {}
            Err(_) => return Err(sent(ProviderErrorKind::Protocol)),
        }
    }
    match std::str::from_utf8(chunk) {
        Ok(_) => Ok(()),
        Err(error) if error.error_len().is_none() => {
            tail.extend_from_slice(&chunk[error.valid_up_to()..]);
            Ok(())
        }
        Err(_) => Err(sent(ProviderErrorKind::Protocol)),
    }
}

struct Observation {
    protocol: String,
    model: String,
    id: Option<String>,
    started: bool,
    finished: bool,
    finish: FinishReason,
    finish_observed: bool,
    secret: String,
    text_started: bool,
    usage: Usage,
    price: Option<gateway_core::metering::ModelPriceOverride>,
}
impl Observation {
    fn new(
        protocol: String,
        model: String,
        secret: String,
        price: Option<gateway_core::metering::ModelPriceOverride>,
    ) -> Self {
        Self {
            protocol,
            model,
            id: None,
            started: false,
            finished: false,
            finish: FinishReason::Stop,
            finish_observed: false,
            secret,
            text_started: false,
            usage: Usage::new(),
            price,
        }
    }
    fn frame(&mut self, frame: SseFrame) -> Result<ProviderEvent, ProviderError> {
        let (raw, events) = frame.into_parts();
        let mut facts = Vec::new();
        let mut parsed = None;
        let mut event_type = None;
        if self.protocol == "openai-chat"
            && std::str::from_utf8(&raw).is_ok_and(gateway_protocol::openai::sse::sse_frame_is_done)
        {
            if !self.started || !self.finish_observed {
                return Err(sent(ProviderErrorKind::Protocol));
            }
            self.finished = true;
            facts.extend(self.completion());
        }
        for event in events {
            // 空 data 与 SSE 注释都只保活，不代表业务输出
            if event.data.trim().is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(&event.data)
                && value.is_object()
            {
                match self.observe(&value, event.event.as_deref(), false) {
                    Ok(observed) => facts.extend(observed),
                    Err(error) => {
                        let mut value = value;
                        let changed = redact_error_value(&mut value, &self.secret);
                        let event_type = event
                            .event
                            .map(|name| name.replace(&self.secret, "[REDACTED]"));
                        // JSON 解析已消除 Unicode 转义，只在含密钥时重编码失败帧，原生正常帧保持原样
                        let redacted_raw = if changed {
                            let prefix = event_type
                                .as_ref()
                                .map(|name| format!("event: {name}\n"))
                                .unwrap_or_default();
                            Bytes::from(format!("{prefix}data: {value}\n\n"))
                        } else {
                            Bytes::from(
                                String::from_utf8_lossy(&raw).replace(&self.secret, "[REDACTED]"),
                            )
                        };
                        let wire = ProtocolWireEvent::json_with_raw_sse_metadata(
                            &self.protocol,
                            event_type,
                            value,
                            redacted_raw,
                            None,
                            None,
                        )
                        .map_err(|_| sent(ProviderErrorKind::Protocol))?;
                        return Err(error.with_atomic_client_events(vec![
                            ProviderEvent::canonical_with_wire(facts, wire),
                        ]));
                    }
                }
                parsed = Some(value);
                event_type = event.event;
            } else {
                // 业务 data 必须是 JSON 对象；[DONE] 已由 SSE 解码器单独处理
                return Err(sent(ProviderErrorKind::Protocol));
            }
        }
        let wire = match parsed {
            Some(value) => ProtocolWireEvent::json_with_raw_sse_metadata(
                &self.protocol,
                event_type,
                value,
                Bytes::from(raw),
                None,
                None,
            ),
            None => ProtocolWireEvent::raw_sse(&self.protocol, Bytes::from(raw)),
        }
        .map_err(|_| sent(ProviderErrorKind::Protocol))?;
        Ok(ProviderEvent::canonical_with_wire(facts, wire))
    }
    fn completion(&self) -> Vec<GatewayEvent> {
        let mut facts = Vec::new();
        if let Some(price) = &self.price
            && let Some(cost) = crate::pricing::calculate(&self.usage, price)
        {
            facts.push(GatewayEvent::CalculatedCost(cost));
        }
        facts.push(GatewayEvent::Completed(self.meta()));
        facts
    }
    fn meta(&self) -> ResponseMeta {
        ResponseMeta::new(self.id.as_deref().unwrap_or(""), &self.model)
            .with_finish_reason(self.finish)
    }
    fn observe(
        &mut self,
        value: &Value,
        event_type: Option<&str>,
        complete: bool,
    ) -> Result<Vec<GatewayEvent>, ProviderError> {
        let typ = event_type
            .or_else(|| value.get("type").and_then(Value::as_str))
            .unwrap_or("");
        if typ == "error"
            || typ == "response.failed"
            || value.get("error").is_some_and(|e| !e.is_null())
        {
            let error = value
                .pointer("/response/error")
                .or_else(|| value.get("error"))
                .unwrap_or(value);
            let kind = match error
                .get("type")
                .or_else(|| error.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("")
            {
                "rate_limit_error" | "rate_limit_exceeded" => ProviderErrorKind::RateLimited,
                "authentication_error" | "invalid_api_key" => ProviderErrorKind::Unauthorized,
                "invalid_request_error" => ProviderErrorKind::InvalidRequest,
                _ => ProviderErrorKind::Unavailable,
            };
            let mut failure = sent(kind);
            if let Some(code) = error.get("code").and_then(Value::as_str) {
                failure = failure.with_upstream_code(OpaqueUpstreamValue::new(
                    code.replace(&self.secret, "[REDACTED]"),
                ));
            }
            if let Some(message) = error.get("message").and_then(Value::as_str) {
                failure = failure.with_client_visible_upstream_error(
                    gateway_core::error::ClientVisibleUpstreamError::new(
                        message.replace(&self.secret, "[REDACTED]"),
                        error
                            .get("code")
                            .and_then(Value::as_str)
                            .map(|value| value.replace(&self.secret, "[REDACTED]")),
                        error
                            .get("type")
                            .and_then(Value::as_str)
                            .map(|value| value.replace(&self.secret, "[REDACTED]")),
                    ),
                );
            }
            return Err(failure);
        }
        let response = value
            .get("response")
            .or_else(|| value.get("message"))
            .unwrap_or(value);
        let mut facts = Vec::new();
        if let Some(id) = response.get("id").and_then(Value::as_str) {
            self.id = Some(id.to_owned());
        }
        if let Some(model) = response.get("model").and_then(Value::as_str) {
            self.model = model.to_owned();
        }
        if !self.started && self.id.is_some() {
            self.started = true;
            facts.push(GatewayEvent::Started(self.meta()));
        }
        let usage = response.get("usage").or_else(|| value.get("usage"));
        if let Some(usage) = usage.filter(|v| v.is_object()) {
            let mut result = Usage::new();
            result.input_tokens = usage
                .get("input_tokens")
                .or_else(|| usage.get("prompt_tokens"))
                .and_then(Value::as_u64);
            result.output_tokens = usage
                .get("output_tokens")
                .or_else(|| usage.get("completion_tokens"))
                .and_then(Value::as_u64);
            result.cached_input_tokens = usage
                .pointer("/input_tokens_details/cached_tokens")
                .or_else(|| usage.pointer("/prompt_tokens_details/cached_tokens"))
                .or_else(|| usage.get("prompt_cache_hit_tokens"))
                .or_else(|| usage.get("cache_read_input_tokens"))
                .and_then(Value::as_u64);
            result.cache_write_input_tokens = usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64);
            result.reasoning_output_tokens = usage
                .pointer("/output_tokens_details/reasoning_tokens")
                .or_else(|| usage.pointer("/completion_tokens_details/reasoning_tokens"))
                .and_then(Value::as_u64);
            result.total_tokens = usage.get("total_tokens").and_then(Value::as_u64);
            // Anthropic input_tokens 不含缓存命中和创建量，统一输入用量保留完整账单语义
            if self.protocol == "anthropic" {
                result.input_tokens = result.input_tokens.map(|n| {
                    n.saturating_add(result.cached_input_tokens.unwrap_or(0))
                        .saturating_add(result.cache_write_input_tokens.unwrap_or(0))
                });
            }
            self.usage.merge(&result);
            facts.push(GatewayEvent::Usage(result));
        }
        let text = match self.protocol.as_str() {
            "openai" if typ == "response.output_text.delta" => value.get("delta"),
            "openai-chat" => value
                .pointer("/choices/0/delta/content")
                .or_else(|| value.pointer("/choices/0/message/content")),
            "anthropic" => value.pointer("/delta/text"),
            _ => None,
        }
        .and_then(Value::as_str);
        if let Some(text) = text {
            if !self.text_started {
                self.text_started = true;
                facts.push(GatewayEvent::ContentAdded(
                    gateway_core::event::ContentItem::new(
                        0,
                        gateway_core::event::ContentKind::Text,
                    ),
                ));
            }
            facts.push(GatewayEvent::TextDelta(TextDelta {
                content_index: 0,
                text: text.to_owned(),
            }));
        }
        let reason = value
            .pointer("/choices/0/finish_reason")
            .or_else(|| value.pointer("/delta/stop_reason"))
            .or_else(|| value.get("stop_reason"))
            .and_then(Value::as_str);
        if let Some(reason) = reason {
            self.finish_observed = true;
            self.finish = match reason {
                "length" | "max_tokens" => FinishReason::Length,
                "tool_calls" | "tool_use" => FinishReason::ToolCall,
                "content_filter" => FinishReason::ContentFilter,
                _ => FinishReason::Stop,
            };
        }
        let terminal = match self.protocol.as_str() {
            "openai" => matches!(typ, "response.completed" | "response.incomplete"),
            "anthropic" => {
                typ == "message_stop"
                    || complete && value.get("stop_reason").is_some_and(|v| !v.is_null())
            }
            "openai-chat" => complete && reason.is_some(),
            _ => false,
        };
        if terminal {
            if !self.started {
                return Err(sent(ProviderErrorKind::Protocol));
            }
            if typ == "response.incomplete" {
                self.finish = match response
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str)
                {
                    Some("max_output_tokens" | "max_tokens") => FinishReason::Length,
                    Some("content_filter") => FinishReason::ContentFilter,
                    _ => FinishReason::Other,
                };
            }
            self.finished = true;
            facts.extend(self.completion());
        }
        Ok(facts)
    }
}

/// 有状态续接和后台执行需要独立上游合同，所有入口在选号及中间件后统一拒绝
pub(crate) fn has_unsupported_stateful_fields(generate: &GenerateRequest) -> bool {
    let body = generate.protocol_payload().body();
    ["previous_response_id", "context_management", "conversation"]
        .iter()
        .any(|key| body.get(*key).is_some_and(|value| !value.is_null()))
        || body.get("background").and_then(Value::as_bool) == Some(true)
}

/// 失败载荷按解码后的字符串脱敏，避免 Unicode 转义绕过原始字节替换
fn redact_error_body(bytes: &[u8], secret: &str) -> Bytes {
    if let Ok(mut value) = serde_json::from_slice::<Value>(bytes)
        && redact_error_value(&mut value, secret)
    {
        return Bytes::from(value.to_string());
    }
    Bytes::from(String::from_utf8_lossy(bytes).replace(secret, "[REDACTED]"))
}

fn redact_error_value(value: &mut Value, secret: &str) -> bool {
    match value {
        Value::String(text) => {
            if text.contains(secret) {
                *text = text.replace(secret, "[REDACTED]");
                true
            } else {
                false
            }
        }
        Value::Array(values) => {
            let mut changed = false;
            for value in values {
                changed |= redact_error_value(value, secret);
            }
            changed
        }
        Value::Object(values) => {
            let mut changed = false;
            for (key, mut value) in std::mem::take(values) {
                changed |= key.contains(secret);
                changed |= redact_error_value(&mut value, secret);
                values.insert(key.replace(secret, "[REDACTED]"), value);
            }
            changed
        }
        _ => false,
    }
}
