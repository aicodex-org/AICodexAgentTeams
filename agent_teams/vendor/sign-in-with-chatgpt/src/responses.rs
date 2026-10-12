//! 官方模型目录与 SSE Responses 合同，不重试分享额度错误或继承租户配置
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use crate::{
    CancellationToken, ChatGPTError, ChatGPTModel, ResponseInput, ResponseText,
    StreamResponseOptions,
    errors::{api_error, cancelled},
    transport::{Transport, json_response, request, send},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;

pub(crate) async fn list_models(
    transport: &dyn Transport,
    access: &str,
    signal: &CancellationToken,
) -> Result<Vec<ChatGPTModel>, ChatGPTError> {
    let mut req = request("GET", "https://api.openai.com/v1/models");
    req.headers
        .insert("authorization".into(), format!("Bearer {access}"));
    req.headers
        .insert("accept".into(), "application/json".into());
    let response = send(transport, req, signal, Duration::from_secs(30)).await?;
    let status = response.status;
    let request_id = response.headers.get("x-request-id").cloned();
    let body = json_response(response, signal, Duration::from_secs(30)).await?;
    if !(200..300).contains(&status) {
        return Err(api_error(Some(&body), Some(status), request_id.as_deref()));
    }
    let invalid = || {
        ChatGPTError::new(
            "invalid_model_catalog",
            "ChatGPT returned an unexpected model catalog. Try again.",
            true,
        )
        .status(status)
    };
    let models = body["models"].as_array().ok_or_else(invalid)?;
    let mut result = Vec::new();
    for model in models {
        if model["visibility"] != "list" {
            continue;
        }
        let valid = |v: &Value| {
            v.as_str()
                .filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= 200)
                .map(str::to_owned)
        };
        let slug = valid(&model["slug"]);
        let display_name = valid(&model["display_name"]);
        if let (Some(slug), Some(display_name)) = (slug, display_name) {
            result.push(ChatGPTModel { slug, display_name });
        } else {
            return Err(ChatGPTError::new(
                "invalid_model_catalog",
                "ChatGPT returned an incomplete model listing. Try again.",
                true,
            )
            .status(status));
        }
    }
    Ok(result)
}

pub(crate) async fn stream_response(
    transport: &dyn Transport,
    access: &str,
    options: &StreamResponseOptions,
    signal: &CancellationToken,
) -> Result<ResponseText, ChatGPTError> {
    let input = match &options.input {
        ResponseInput::Text(text) => json!([{"role":"user","content":text}]),
        ResponseInput::Messages(messages) => {
            serde_json::to_value(messages).expect("serializable messages")
        }
    };
    let mut body = json!({"model":options.model,"input":input,"store":false,"stream":true});
    if let Some(instructions) = &options.instructions {
        body["instructions"] = instructions.as_str().into();
    }
    let mut req = request("POST", "https://api.openai.com/v1/responses");
    req.headers
        .insert("authorization".into(), format!("Bearer {access}"));
    req.headers
        .insert("accept".into(), "text/event-stream".into());
    req.headers
        .insert("content-type".into(), "application/json".into());
    req.body = serde_json::to_vec(&body).expect("serializable request");
    let operation = async {
        let mut response = send(transport, req, signal, Duration::from_secs(180)).await?;
        let status = response.status;
        let request_id = response.headers.get("x-request-id").cloned();
        if !(200..300).contains(&status) {
            let body = json_response(response, signal, Duration::from_secs(180)).await?;
            return Err(api_error(Some(&body), Some(status), request_id.as_deref()));
        }
        let content_type = response.headers.get("content-type").map(|s| {
            s.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
        });
        if content_type
            .as_ref()
            .is_some_and(|s| !s.is_empty() && s != "text/event-stream")
        {
            return Err(ChatGPTError::new(
                "invalid_stream",
                "ChatGPT did not return the expected response stream.",
                true,
            )
            .status(status));
        }
        let mut parser = SseParser::default();
        let mut text = String::new();
        let mut text_units = 0usize;
        loop {
            let chunk = tokio::select! { biased; _=signal.cancelled()=>return Err(cancelled()), chunk=response.body.next()=>chunk };
            let Some(chunk) = chunk else {
                break;
            };
            let chunk = chunk.map_err(|_| {
                if signal.is_cancelled() {
                    cancelled()
                } else {
                    connection_interrupted()
                }
            })?;
            parser.push(&chunk);
            while let Some((name, data)) = parser.next_event() {
                if signal.is_cancelled() {
                    return Err(cancelled());
                }
                if data == "[DONE]" {
                    return Err(interrupted());
                }
                let event: Value = serde_json::from_str(&data).map_err(|_| {
                    ChatGPTError::new(
                        "invalid_stream",
                        "The response stream contained an invalid event. Try again.",
                        true,
                    )
                })?;
                if name.as_deref().is_some_and(|s| s.starts_with("thread.")) {
                    continue;
                }
                if name.as_deref() == Some("error")
                    || event
                        .get("error")
                        .is_some_and(|v| !v.is_null() && v != false && v != "")
                {
                    let detail = event
                        .get("error")
                        .filter(|v| !v.is_null())
                        .unwrap_or(&event);
                    let detail = if detail.is_string() {
                        json!({"error":detail})
                    } else {
                        detail.clone()
                    };
                    return Err(api_error(
                        Some(&detail),
                        Some(status),
                        request_id.as_deref(),
                    ));
                }
                match event["type"].as_str() {
                    Some("response.output_text.delta") => {
                        if let Some(delta) = event["delta"].as_str() {
                            text_units += delta.encode_utf16().count();
                            if text_units > 16 * 1024 * 1024 {
                                return Err(ChatGPTError::new(
                                    "response_too_large",
                                    "The response was too large. Try a smaller request.",
                                    false,
                                ));
                            }
                            text.push_str(delta);
                            if let Some(on_delta) = &options.on_delta {
                                on_delta(delta);
                            }
                        }
                    }
                    Some("response.failed") => {
                        return Err(api_error(
                            event.get("response"),
                            Some(status),
                            request_id.as_deref(),
                        ));
                    }
                    Some("error") => {
                        return Err(api_error(Some(&event), Some(status), request_id.as_deref()));
                    }
                    Some("response.incomplete") => {
                        return Err(ChatGPTError::new(
                            "response_incomplete",
                            "ChatGPT stopped before completing the response. You can keep the partial text or try again.",
                            true,
                        ));
                    }
                    Some("response.completed") => {
                        if signal.is_cancelled() {
                            return Err(cancelled());
                        }
                        return Ok(ResponseText { text });
                    }
                    _ => {}
                }
            }
        }
        if signal.is_cancelled() {
            Err(cancelled())
        } else {
            Err(interrupted())
        }
    };
    tokio::select! {
        biased;
        _=signal.cancelled()=>Err(cancelled()),
        result=tokio::time::timeout(Duration::from_secs(180),operation)=>result.unwrap_or_else(|_|Err(connection_interrupted())),
    }
}
fn interrupted() -> ChatGPTError {
    ChatGPTError::new(
        "stream_interrupted",
        "The response ended before completion. You can keep the partial text or try again.",
        true,
    )
}
fn connection_interrupted() -> ChatGPTError {
    ChatGPTError::new(
        "stream_interrupted",
        "The connection was interrupted. You can keep the partial text or try again.",
        true,
    )
}

// 在字节层切行以保留分块 UTF-8 与 CRLF，空行才交付一个完整 SSE 事件
#[derive(Default)]
struct SseParser {
    pending: Vec<u8>,
    data: Vec<String>,
    skip_lf: bool,
    event: Option<String>,
}
impl SseParser {
    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }
    fn next_event(&mut self) -> Option<(Option<String>, String)> {
        loop {
            if self.skip_lf {
                if self.pending.is_empty() {
                    return None;
                }
                if self.pending[0] == b'\n' {
                    self.pending.remove(0);
                }
                self.skip_lf = false;
            }
            let at = self
                .pending
                .iter()
                .position(|c| *c == b'\r' || *c == b'\n')?;
            let bytes: Vec<_> = self.pending.drain(..=at).collect();
            self.skip_lf = bytes[at] == b'\r';
            let line = String::from_utf8_lossy(&bytes[..at]);
            if line.is_empty() {
                if self.data.is_empty() && self.event.is_none() {
                    continue;
                }
                return Some((self.event.take(), std::mem::take(&mut self.data).join("\n")));
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((&line, ""));
            if field == "data" {
                self.data
                    .push(value.strip_prefix(' ').unwrap_or(value).into());
            }
            if field == "event" {
                self.event = Some(value.strip_prefix(' ').unwrap_or(value).into());
            }
        }
    }
}
