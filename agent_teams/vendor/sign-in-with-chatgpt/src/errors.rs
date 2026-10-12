//! 官方错误映射与不包含服务端敏感内容的诊断投影
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGPTError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_shape: Option<Box<str>>,
}

impl ChatGPTError {
    pub fn new(code: &str, message: &str, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
            status: None,
            request_id: None,
            param: None,
            response_shape: None,
        }
    }
    pub(crate) fn status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }
}
impl std::fmt::Display for ChatGPTError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ChatGPTError {}

pub(crate) fn cancelled() -> ChatGPTError {
    ChatGPTError::new("cancelled", "The request was cancelled.", false)
}
pub(crate) fn network_error() -> ChatGPTError {
    ChatGPTError::new(
        "network_error",
        "Could not reach ChatGPT. Check your connection and try again.",
        true,
    )
}
pub(crate) fn requires_reauthentication(error: &ChatGPTError) -> bool {
    [
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
    ]
    .contains(&error.code.as_str())
}

fn safe_identifier(value: Option<&str>, max: usize) -> Option<String> {
    value
        .filter(|s| {
            !s.is_empty()
                && s.len() <= max
                && (s.as_bytes()[0].is_ascii_alphanumeric() || s.as_bytes()[0] == b'_')
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.:[]-".contains(&c))
        })
        .map(str::to_owned)
}

fn shape(value: &Value, depth: usize) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(_) => "boolean".into(),
        Value::String(_) => "string".into(),
        Value::Number(_) => "number".into(),
        Value::Array(items) if depth < 3 => format!(
            "[{}]",
            items
                .first()
                .map(|v| shape(v, depth + 1))
                .unwrap_or_default()
        ),
        Value::Array(_) => "array".into(),
        Value::Object(_) if depth >= 3 => "object".into(),
        Value::Object(fields) => {
            let allowed = [
                "error",
                "detail",
                "code",
                "message",
                "type",
                "param",
                "loc",
                "input",
                "ctx",
                "response",
                "status",
                "request_id",
            ];
            let mut known: Vec<String> = fields
                .iter()
                .filter(|(k, _)| allowed.contains(&k.as_str()))
                .take(12)
                .map(|(k, v)| {
                    format!(
                        "{k}:{}",
                        if k == "input" || k == "ctx" {
                            "redacted".into()
                        } else {
                            shape(v, depth + 1)
                        }
                    )
                })
                .collect();
            let others = fields.len() - known.len();
            if others > 0 {
                known.push(format!("other:{others}"));
            }
            format!("{{{}}}", known.join(","))
        }
    }
}

fn invalid_request(detail: &Value) -> String {
    let issues = detail
        .get("detail")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect::<Vec<_>>())
        .unwrap_or_else(|| vec![detail]);
    for issue in issues {
        let locations = issue
            .get("loc")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect::<Vec<_>>())
            .unwrap_or_else(|| issue.get("param").into_iter().collect());
        for field in locations.iter().filter_map(|v| v.as_str()) {
            let display = match field {
                "input" if issue["type"] == "list_type" => return "ChatGPT requires input as a list of messages. Update the app's request format.".into(),
                "input" | "model" | "instructions" | "tools" => field,
                "store" => "storage setting", "stream" => "streaming setting",
                "background" => "background setting", "conversation" => "conversation setting",
                _ => continue,
            };
            return format!(
                "ChatGPT rejected the request's {display}. Check its format and supported values."
            );
        }
    }
    "ChatGPT rejected this request. Check its input, model, and supported options.".into()
}

pub(crate) fn api_error(
    body: Option<&Value>,
    status: Option<u16>,
    request_id: Option<&str>,
) -> ChatGPTError {
    let empty = serde_json::json!({});
    let mut detail = body.filter(|v| v.is_object()).unwrap_or(&empty);
    for _ in 0..4 {
        if detail["error"].is_object() {
            detail = &detail["error"];
        } else if detail["detail"].is_object() {
            detail = &detail["detail"];
        } else {
            break;
        }
    }
    let code = safe_identifier(
        detail["error"].as_str().or_else(|| detail["code"].as_str()),
        100,
    )
    .unwrap_or("api_error".into());
    let canonical = match code.as_str() {
        "subscription_sharing_v2_user_not_eligible" => "subscription_sharing_user_not_eligible",
        "subscription_sharing_v2_route_not_supported" => "subscription_sharing_route_not_supported",
        "subscription_sharing_v2_invalid_user" => "subscription_sharing_invalid_user",
        "subscription_sharing_v2_user_unavailable" => "subscription_sharing_user_unavailable",
        other => other,
    };
    let (message, retryable) = match canonical {
        "subscription_sharing_user_not_eligible" => ("Token sharing is not available for this ChatGPT account or workspace.".into(), false),
        "subscription_sharing_usage_limit_exceeded" => ("A ChatGPT sharing limit has been reached. Check your usage and app limits in ChatGPT Settings.".into(), false),
        "subscription_sharing_usage_unavailable" => ("ChatGPT usage could not be checked. Try again shortly.".into(), true),
        "subscription_sharing_unsupported_capability" => ("This request includes a capability that token sharing does not support.".into(), false),
        "subscription_sharing_route_not_supported" => ("This request uses a route that token sharing does not support.".into(), false),
        "subscription_sharing_invalid_user" => ("ChatGPT could not validate the selected account's subscription context. Check this connection and its granted permissions.".into(), false),
        "subscription_sharing_user_unavailable" => ("Your ChatGPT account or workspace is temporarily unavailable. Try again shortly.".into(), true),
        "chatpass_v2_scope_not_authorized" => ("The granted ChatGPT permissions do not authorize this request. Check the app and grant configuration.".into(), false),
        "chatpass_v2_invalid_authorization_context" => ("The ChatGPT authorization context does not permit this request. Check the app and grant configuration.".into(), false),
        "invalid_client" => ("ChatGPT rejected the app's client configuration. Check the saved registration before trying again.".into(), false),
        "invalid_grant" => ("ChatGPT did not accept this authorization grant. Start sign-in again with the saved connection.".into(), false),
        "invalid_refresh_token" | "token_expired" | "refresh_token_expired" | "refresh_token_invalidated" | "refresh_token_reused" => ("Your ChatGPT connection can no longer be renewed. Sign in again.".into(), false),
        "invalid_token" | "invalid_api_key" => ("ChatGPT did not accept this credential. Check the selected connection and its granted permissions.".into(), false),
        "access_denied" => ("Sign-in was not completed. You can try again when you are ready.".into(), false),
        "model_not_found" => ("This model is not available for your ChatGPT connection. Choose a supported model.".into(), false),
        "subscription_sharing_v2_client_not_enabled" => ("This app is not enabled for token sharing. Contact the app developer.".into(), false),
        _ if matches!(status, Some(400 | 422)) || ["invalid_request_error","invalid_request"].contains(&code.as_str()) => (invalid_request(detail), false),
        _ if status == Some(401) => ("ChatGPT did not accept the selected connection's signed identity or direct permission. Check the profile and granted scopes.".into(), false),
        _ if status == Some(403) => ("A ChatGPT policy or permission restriction prevented this request. Check the app's configuration and permitted serving region.".into(), false),
        _ if status == Some(429) => ("Too many requests. Wait a moment before trying again.".into(), true),
        _ if status.is_some_and(|s| s >= 500) => ("ChatGPT is temporarily unavailable. Try again shortly.".into(), true),
        _ => ("ChatGPT could not complete this request.".into(), false),
    };
    ChatGPTError {
        code,
        message,
        retryable,
        status,
        request_id: safe_identifier(request_id, 160).map(Into::into),
        param: safe_identifier(detail["param"].as_str(), 160).map(Into::into),
        response_shape: Some(
            body.map(|v| shape(v, 0))
                .unwrap_or_else(|| "undefined".into())
                .into(),
        ),
    }
}
