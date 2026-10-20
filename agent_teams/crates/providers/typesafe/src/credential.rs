//! TypeSafe API Key 材料与地址校验，轮换和导出保留未知字段

use gateway_admin::{
    model::provider_credentials::ProviderDocument,
    ports::provider::{ProviderAdminError, ProviderAdminErrorKind},
};
use gateway_core::account::OpaqueProviderData;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use url::Url;

pub const PROTOCOLS: [&str; 1] = ["typesafe-systemone"];
fn default_base() -> String {
    "https://api.typesafe.ai".to_owned()
}
fn default_protocols() -> BTreeSet<String> {
    PROTOCOLS.into_iter().map(str::to_owned).collect()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TypeSafeCredential {
    pub(crate) api_key: String,
    #[serde(default = "default_base")]
    pub(crate) base_url: String,
    #[serde(default = "default_protocols")]
    pub(crate) protocols: BTreeSet<String>,
    #[serde(flatten)]
    pub(crate) extra: Map<String, Value>,
}
impl std::fmt::Debug for TypeSafeCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TypeSafeCredential([REDACTED])")
    }
}
impl TypeSafeCredential {
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
            || credential.api_key.trim() != credential.api_key
            || credential.protocols != default_protocols()
        {
            return Err(invalid());
        }
        let url = Url::parse(&credential.base_url).map_err(|_| invalid())?;
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
        Ok(format!("{}/v1/systemone", self.base_url))
    }
    pub(crate) fn document(&self) -> Result<ProviderDocument, ProviderAdminError> {
        document(serde_json::to_value(self).map_err(|_| invalid())?)
    }
    pub(crate) fn configuration(&self) -> Result<ProviderDocument, ProviderAdminError> {
        document(
            serde_json::json!({"base_url":self.base_url,"protocols":self.protocols,"transport":"http"}),
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
