//! MiMo 凭据与原生协议端点，保留自定义中转前缀并独立校验模式与协议

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

fn default_protocols() -> BTreeSet<String> {
    PROTOCOLS.into_iter().map(str::to_owned).collect()
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AccessMode {
    #[default]
    ApiKey,
    TokenPlan,
}

/// 密钥仅供 Provider 使用，普通账号投影不包含认证材料
#[derive(Clone, Serialize, Deserialize)]
pub struct MiMoCredential {
    pub(crate) api_key: String,
    #[serde(default)]
    access_mode: AccessMode,
    #[serde(default)]
    pub(crate) base_url: String,
    #[serde(default = "default_protocols")]
    pub(crate) protocols: BTreeSet<String>,
}

impl std::fmt::Debug for MiMoCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MiMoCredential([REDACTED])")
    }
}

impl MiMoCredential {
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
            || credential.api_key.chars().any(char::is_whitespace)
            || credential.api_key.chars().any(char::is_control)
            || credential.protocols.is_empty()
            || credential
                .protocols
                .iter()
                .any(|p| !PROTOCOLS.contains(&p.as_str()))
        {
            return Err(invalid());
        }
        // 导入只有密钥时识别个人与团队套餐，显式选择始终是管理端权威配置
        if !value.contains_key("access_mode")
            && (credential.api_key.starts_with("tp-") || credential.api_key.starts_with("ttp-"))
        {
            credential.access_mode = AccessMode::TokenPlan;
        }
        if credential.base_url.is_empty() {
            credential.base_url = match credential.access_mode {
                AccessMode::ApiKey => "https://api.xiaomimimo.com/v1",
                AccessMode::TokenPlan => "https://token-plan-cn.xiaomimimo.com/v1",
            }
            .to_owned();
        }
        let mut url = Url::parse(&credential.base_url).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }
        // 官方根地址统一保存为 OpenAI Base URL；未知路径和中转前缀保持原样
        if matches!(url.path().trim_end_matches('/'), "" | "/v1")
            || official_host(&url)
                && matches!(
                    url.path().trim_end_matches('/'),
                    "/anthropic" | "/anthropic/v1"
                )
        {
            url.set_path("/v1");
        }
        credential.base_url = url.as_str().trim_end_matches('/').to_owned();
        Ok(credential)
    }

    pub fn endpoint(&self, protocol: &str) -> Result<String, ProviderAdminError> {
        if !self.protocols.contains(protocol) {
            return Err(invalid());
        }
        match protocol {
            "openai" => Ok(format!("{}/responses", self.base_url)),
            "openai-chat" => Ok(format!("{}/chat/completions", self.base_url)),
            "anthropic" => {
                let mut url = Url::parse(&self.base_url).map_err(|_| invalid())?;
                if official_host(&url) && url.path() == "/v1" {
                    url.set_path("/anthropic/v1/messages");
                    Ok(url.to_string())
                } else {
                    // 自定义地址是 OpenAI API Base URL，Messages 使用同一前缀下的原生端点
                    Ok(format!("{}/messages", self.base_url))
                }
            }
            _ => Err(invalid()),
        }
    }

    pub(crate) fn document(&self) -> Result<ProviderDocument, ProviderAdminError> {
        document(serde_json::to_value(self).map_err(|_| invalid())?)
    }

    pub(crate) fn configuration(&self) -> Result<ProviderDocument, ProviderAdminError> {
        document(serde_json::json!({
            "access_mode": self.access_mode,
            "base_url": self.base_url,
            "protocols": self.protocols,
            "transport": "http"
        }))
    }
}

fn official_host(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some(
            "api.xiaomimimo.com"
                | "token-plan-cn.xiaomimimo.com"
                | "token-plan-sgp.xiaomimimo.com"
                | "token-plan-ams.xiaomimimo.com"
        )
    )
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
