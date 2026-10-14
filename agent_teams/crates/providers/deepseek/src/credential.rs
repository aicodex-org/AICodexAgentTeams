//! 密钥文档校验与三协议根地址派生，配置和实际验证结果各自独立

use gateway_admin::{
    model::provider_credentials::ProviderDocument,
    ports::provider::{ProviderAdminError, ProviderAdminErrorKind},
};
use gateway_core::account::OpaqueProviderData;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use url::Url;

pub const PROTOCOLS: [&str; 3] = ["openai", "openai-chat", "anthropic"];
fn default_base() -> String {
    "https://api.deepseek.com".to_owned()
}
fn default_protocols() -> BTreeSet<String> {
    PROTOCOLS.into_iter().map(str::to_owned).collect()
}

/// 密钥只在 Provider 内使用，Debug 不打印认证内容
#[derive(Clone, Serialize, Deserialize)]
pub struct DeepSeekCredential {
    pub(crate) api_key: String,
    #[serde(default = "default_base")]
    pub(crate) base_url: String,
    #[serde(default = "default_protocols")]
    pub(crate) protocols: BTreeSet<String>,
}
impl std::fmt::Debug for DeepSeekCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeepSeekCredential([REDACTED])")
    }
}
impl DeepSeekCredential {
    pub fn parse(value: &Map<String, Value>) -> Result<Self, ProviderAdminError> {
        if value
            .get("transport")
            .is_some_and(|v| v.as_str() != Some("http"))
        {
            return Err(invalid());
        }
        let mut credential: Self =
            serde_json::from_value(Value::Object(value.clone())).map_err(|_| invalid())?;
        if credential.api_key.trim().is_empty()
            || credential.api_key.chars().any(char::is_control)
            || credential.protocols.is_empty()
            || credential
                .protocols
                .iter()
                .any(|p| !PROTOCOLS.contains(&p.as_str()))
        {
            return Err(invalid());
        }
        let url = Url::parse(&credential.base_url).map_err(|_| invalid())?;
        // 自定义地址允许路径前缀，但必须是根地址；路径中的 /v1 不隐式重写
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        credential.base_url = url.as_str().trim_end_matches('/').to_owned();
        Ok(credential)
    }
    pub fn endpoint(&self, protocol: &str) -> Result<String, ProviderAdminError> {
        if !self.protocols.contains(protocol) {
            return Err(invalid());
        }
        let path = match protocol {
            "openai" => "/responses",
            "openai-chat" => "/v1/chat/completions",
            "anthropic" => "/anthropic/v1/messages",
            _ => return Err(invalid()),
        };
        Ok(format!("{}{path}", self.base_url))
    }
    pub(crate) fn document(&self) -> Result<ProviderDocument, ProviderAdminError> {
        document(serde_json::to_value(self).map_err(|_| invalid())?)
    }
    pub(crate) fn configuration(&self) -> Result<ProviderDocument, ProviderAdminError> {
        document(
            serde_json::json!({"base_url": self.base_url, "protocols": self.protocols, "transport": "http"}),
        )
    }
}
pub(crate) fn document(value: Value) -> Result<ProviderDocument, ProviderAdminError> {
    let Value::Object(object) = value else {
        return Err(invalid());
    };
    Ok(ProviderDocument::new(OpaqueProviderData::new(object)))
}
pub(crate) fn invalid() -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::Invalid)
}
pub(crate) fn unavailable() -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::Unavailable)
}
