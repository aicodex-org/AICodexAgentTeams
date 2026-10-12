//! ChatGPT OAuth 的 SDK 连接记录、账号事实与公共 API 认证，不解释 Codex 凭据

use super::admin::{CodexCredentialAdminError, PreparedCodexCredentialRotation};
use super::{CodexCredentialCodec, CodexCredentialData, CodexOAuthSecret};
use gateway_core::account::{
    CredentialCasUpdate, CredentialRevision, CredentialState, LoadedCredential, NewProviderAccount,
    ProviderAccount, ProviderAccountId, ProviderAccountUpdate, QuotaState,
};
use gateway_core::routing::ProviderKind;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sign_in_with_chatgpt::{
    ChatGPTError,
    hosted::{OAuthClient, sharing_enabled, validate_connection},
    transport::{ReqwestTransport, Transport},
};
use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

pub const AUTHENTICATION_KIND: &str = "chatgpt_oauth";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatGptCredentialData {
    pub schema_version: u32,
    pub installation_id: String,
    pub chatgpt_connection: Value,
}
impl std::fmt::Debug for ChatGptCredentialData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChatGptCredentialData([REDACTED])")
    }
}
impl ChatGptCredentialData {
    pub fn validate(&self) -> bool {
        self.schema_version == 1
            && validate_connection(&self.chatgpt_connection).is_ok()
            && self.chatgpt_connection["subject"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
    }
    pub fn ready(&self) -> bool {
        self.chatgpt_connection["status"] == "connected"
            && sharing_enabled(&self.chatgpt_connection)
            && self.chatgpt_connection.get("pendingRefresh").is_none()
            && self.chatgpt_connection["credentials"]["accessToken"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
    }
    pub fn secret(&self) -> CodexOAuthSecret {
        let credentials = &self.chatgpt_connection["credentials"];
        CodexOAuthSecret {
            access_token: SecretString::from(
                credentials["accessToken"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            ),
            refresh_token: credentials["refreshToken"]
                .as_str()
                .map(|s| SecretString::from(s.to_owned())),
            id_token: self.chatgpt_connection["profileIdToken"]
                .as_str()
                .map(|s| SecretString::from(s.to_owned())),
        }
    }
    fn upstream_identity(&self) -> Option<String> {
        // Store 的账号唯一键不含认证类型，隔离与 Codex 相同的 OIDC subject
        self.chatgpt_connection["subject"]
            .as_str()
            .map(|subject| format!("chatgpt:{subject}"))
    }
    pub fn expires_at(&self) -> Option<SystemTime> {
        // 轮换后旧令牌已不可执行，让 worker 立即恢复 checkpoint，而不是等旧到期时间
        let expires = self.chatgpt_connection.get("pendingRefresh").map_or(
            &self.chatgpt_connection["credentials"]["expiresAt"],
            |pending| &pending["receivedAt"],
        );
        expires
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.0)
            .and_then(|v| Duration::try_from_secs_f64(v / 1000.0).ok())
            .and_then(|d| SystemTime::UNIX_EPOCH.checked_add(d))
    }
    pub fn refresh_after(&self) -> Option<SystemTime> {
        if self.chatgpt_connection.get("pendingRefresh").is_some() {
            return None;
        }
        let value = &self.chatgpt_connection["credentials"]["earliestRefreshAt"];
        if let Some(value) = value.as_str() {
            return chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .map(SystemTime::from);
        }
        value
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.0)
            .and_then(|v| Duration::try_from_secs_f64(v).ok())
            .and_then(|d| SystemTime::UNIX_EPOCH.checked_add(d))
    }
    pub fn state(&self) -> CredentialState {
        if self.ready() {
            CredentialState::Ready
        } else {
            CredentialState::Unknown
        }
    }
    pub fn prepare_create(
        self,
        name: String,
    ) -> Result<NewProviderAccount, CodexCredentialAdminError> {
        let bad = || CodexCredentialAdminError::InvalidCredential;
        let account = ProviderAccount::new(
            ProviderAccountId::new(format!("acct_{}", uuid::Uuid::now_v7().simple()))
                .map_err(|_| bad())?,
            ProviderKind::new("openai").map_err(|_| bad())?,
            name,
            self.upstream_identity(),
            AUTHENTICATION_KIND.into(),
            CredentialRevision::new(1).map_err(|_| bad())?,
            self.expires_at(),
        )
        .with_profile(
            self.chatgpt_connection["identity"]["email"]
                .as_str()
                .map(str::to_owned),
            None,
            None,
        )
        .with_account_facts(true, self.state(), QuotaState::unknown(), None, None)
        .with_refresh_schedule(self.secret().refresh_token.is_some(), self.refresh_after());
        let credential = CodexCredentialCodec::encode_complete(CodexCredentialData::ChatGpt(self))
            .map_err(|_| bad())?;
        Ok(NewProviderAccount {
            account,
            credential,
            model_access: None,
        })
    }
    pub fn prepare_rotation(
        self,
        current: LoadedCredential,
    ) -> Result<PreparedCodexCredentialRotation, CodexCredentialAdminError> {
        let bad = || CodexCredentialAdminError::InvalidCredential;
        if current.account.authentication_kind() != AUTHENTICATION_KIND
            || current.account.upstream_user_id() != self.upstream_identity().as_deref()
        {
            return Err(bad());
        }
        let profile = ProviderAccountUpdate {
            account_id: current.account.id().clone(),
            name: current.account.name().into(),
            email: current.account.email().map(str::to_owned),
            plan_type: None,
        };
        let state = self.state();
        let expires = self.expires_at();
        let refresh_after = self.refresh_after();
        let refreshable = self.secret().refresh_token.is_some();
        let credential = CodexCredentialCodec::encode_complete(CodexCredentialData::ChatGpt(self))
            .map_err(|_| bad())?;
        let credential = CredentialCasUpdate::new(
            current.account.id().clone(),
            current.account.revision(),
            profile.clone(),
            credential,
            refreshable,
            expires,
            refresh_after,
        )
        .map_err(|_| bad())?
        .preserving_profile()
        .with_account_state(state, SystemTime::now(), None, None);
        Ok(PreparedCodexCredentialRotation {
            profile,
            credential,
            replacement_identity: None,
            refresh_guards: None,
        })
    }
}

pub fn oauth_client(
    proxy: Option<&gateway_core::account::OutboundProxy>,
) -> Result<OAuthClient, ChatGPTError> {
    let client = crate::transport::client::build_account_http_client("", proxy).map_err(|_| {
        ChatGPTError::new(
            "network_error",
            "ChatGPT 授权连接无法建立，请检查出站配置",
            true,
        )
    })?;
    let transport: Arc<dyn Transport> = Arc::new(ReqwestTransport::from_client(client));
    Ok(OAuthClient::new(transport))
}

/// 刷新锁由调用方持有，先落盘 SDK successor checkpoint，再验证新身份并准备最终 CAS
pub async fn prepare_refresh(
    repository: &super::CodexCredentialRepository,
    account: &ProviderAccount,
) -> Result<PreparedCodexCredentialRotation, ChatGPTError> {
    use gateway_core::account::CredentialCasOutcome;
    let failure = || ChatGPTError::new("storage_unavailable", "ChatGPT 凭据暂时无法保存", true);
    let current = repository
        .store()
        .load_credential(account.id(), account.revision())
        .await
        .map_err(|_| failure())?;
    let CodexCredentialData::ChatGpt(data) =
        CodexCredentialCodec::decode_complete(&current.credential).map_err(|_| failure())?
    else {
        return Err(failure());
    };
    if data.chatgpt_connection.get("pendingRefresh").is_none()
        && data
            .refresh_after()
            .is_some_and(|after| after > SystemTime::now())
    {
        return Err(ChatGPTError::new(
            "refresh_not_due",
            "尚未到达 ChatGPT 允许刷新的时间",
            true,
        ));
    }
    let client = oauth_client(current.account.outbound_proxy())?;
    let state = Arc::new(tokio::sync::Mutex::new((current, data.clone())));
    let record = client
        .refresh(&data.chatgpt_connection, |checkpoint| {
            let state = Arc::clone(&state);
            async move {
                let mut state = state.lock().await;
                let mut next = state.1.clone();
                next.chatgpt_connection["pendingRefresh"] = checkpoint;
                let update = next
                    .clone()
                    .prepare_rotation(state.0.clone())
                    .map_err(|_| failure())?;
                match repository
                    .store()
                    .compare_and_swap_credential(update.credential)
                    .await
                    .map_err(|_| failure())?
                {
                    CredentialCasOutcome::Updated(revision) => {
                        state.0 = repository
                            .store()
                            .load_credential(account.id(), revision)
                            .await
                            .map_err(|_| failure())?;
                        state.1 = next;
                        Ok(())
                    }
                    CredentialCasOutcome::Conflict => Err(failure()),
                }
            }
        })
        .await;
    let mut state = state.lock().await;
    let record = match record {
        Ok(record) => record,
        Err(error) => {
            if sign_in_with_chatgpt::hosted::refresh_requires_reauthentication(&error) {
                repository
                    .apply_state(
                        &state.0.account,
                        CredentialState::Expired,
                        SystemTime::now(),
                    )
                    .await
                    .map_err(|_| failure())?;
            }
            return Err(error);
        }
    };
    state.1.chatgpt_connection = record;
    state
        .1
        .clone()
        .prepare_rotation(state.0.clone())
        .map_err(|_| failure())
}
