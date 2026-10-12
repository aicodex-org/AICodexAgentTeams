//! SDK 公共配置与可安全传递到界面的会话合同
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use crate::{CancellationToken, ChatGPTError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    ReauthRequired,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub status: SessionStatus,
    pub sharing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<SessionIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ChatGPTError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginProfile {
    pub id: String,
    pub label: String,
    pub status: SessionStatus,
    pub sharing: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<SessionIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_new_registration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGPTModel {
    pub slug: String,
    pub display_name: String,
}

/// 由可信本地进程实现的 OS 加密接口，生产实现不得使用明文或硬编码密钥
#[async_trait]
pub trait CredentialEncryption: Send + Sync {
    fn id(&self) -> &str;
    async fn is_available(&self) -> bool;
    async fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>, ChatGPTError>;
    async fn decrypt(&self, ciphertext: &[u8]) -> Result<String, ChatGPTError>;
}

#[async_trait]
pub trait BrowserOpener: Send + Sync {
    async fn open(&self, url: &str) -> Result<(), ChatGPTError>;
}

#[derive(Clone)]
pub struct ChatGPTConfig {
    pub app_name: String,
    pub app_id: String,
    pub redirect_port: u16,
    pub storage_dir: Option<PathBuf>,
    pub credential_encryption: Arc<dyn CredentialEncryption>,
    pub open_browser: Option<Arc<dyn BrowserOpener>>,
    pub send_host_id: bool,
}

#[derive(Clone, Default)]
pub struct SignInOptions {
    pub signal: CancellationToken,
    pub new_profile: bool,
    pub profile_id: Option<String>,
    pub label: Option<String>,
    pub reconsent: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    Developer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseInputMessage {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseInput {
    Text(String),
    Messages(Vec<ResponseInputMessage>),
}

pub type DeltaCallback = Arc<dyn Fn(&str) + Send + Sync>;

pub struct StreamResponseOptions {
    pub model: String,
    pub input: ResponseInput,
    pub instructions: Option<String>,
    pub signal: CancellationToken,
    pub on_delta: Option<DeltaCallback>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseText {
    pub text: String,
}
