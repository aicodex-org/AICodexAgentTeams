//! 宿主管理回调与凭据存储时的 OAuth 适配，复用 SDK 的 OIDC 和轮换验证
//! 宿主须绑定管理员、校验过期时间并独占领取授权事务，不能把令牌交给浏览器

use crate::{
    CancellationToken, ChatGPTError,
    oauth::{OAuth, merge, token_fields},
    storage::{saved_at, validate_record},
    transport::Transport,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{future::Future, sync::Arc};
use subtle::ConstantTimeEq;
use url::Url;

pub const API_BASE_URL: &str = "https://api.openai.com/v1";
pub const REDIRECT_URI: &str = "http://127.0.0.1:1456/auth/callback";
// 官方 OIDC discovery 公布的浏览器入口，生成链接无需服务端预先联网
const AUTHORIZATION_ENDPOINT: &str = "https://auth.openai.com/api/accounts/authorize";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

#[derive(Clone, Serialize, Deserialize)]
pub struct Authorization {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub host_id: String,
    pub client_id: Option<String>,
}

pub struct OAuthClient(OAuth);
impl OAuthClient {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        Self(OAuth::new(transport))
    }

    pub fn authorization_url(
        request: &Authorization,
        app_name: &str,
    ) -> Result<String, ChatGPTError> {
        let mut url = Url::parse(AUTHORIZATION_ENDPOINT).map_err(|_| invalid())?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(request.verifier.as_bytes()));
        url.query_pairs_mut().extend_pairs([
            (
                "client_id",
                request
                    .client_id
                    .as_deref()
                    .unwrap_or("dynamic_agent_client"),
            ),
            ("response_type", "code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("resource", API_BASE_URL),
            ("state", &request.state),
            ("nonce", &request.nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
            ("ext_agent_host_id", &request.host_id),
        ]);
        if request.client_id.is_none() {
            url.query_pairs_mut()
                .append_pair("agent_name_hint", app_name);
        } else {
            url.query_pairs_mut().append_pair("prompt", "consent");
        }
        Ok(url.into())
    }

    pub async fn complete(
        &self,
        request: &Authorization,
        callback: &str,
        previous: Option<&Value>,
    ) -> Result<Value, ChatGPTError> {
        let (code, client) = callback_values(request, callback)?;
        let data = self
            .0
            .token_request(
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", &client),
                    ("code", &code),
                    ("code_verifier", &request.verifier),
                    ("redirect_uri", REDIRECT_URI),
                    ("resource", API_BASE_URL),
                ],
                &CancellationToken::new(),
            )
            .await?;
        let token = data["id_token"].as_str().ok_or_else(invalid)?;
        let verified = self
            .0
            .verify_identity(token, &client, Some(&request.nonce), None)
            .await
            .map_err(|error| {
                // 授权码已经消费，暂时的 JWKS 故障也不能承诺重试同一回调
                if error.retryable {
                    ChatGPTError::new(
                        "authorization_restart_required",
                        "授权码已换取令牌，但身份验证未完成，请重新发起授权",
                        false,
                    )
                } else {
                    error
                }
            })?;
        if previous.is_some_and(|p| p["subject"] != verified["subject"] || p["clientId"] != client)
        {
            return Err(invalid());
        }
        let mut record = json!({"version":1,"clientId":client,"status":"connected","savedAt":saved_at(),"profileIdToken":token});
        merge(&mut record, &verified);
        merge(&mut record, &token_fields(&data, false)?);
        validate_record(&record)?;
        Ok(record)
    }

    pub async fn refresh<F, Fut>(&self, record: &Value, persist: F) -> Result<Value, ChatGPTError>
    where
        F: FnMut(Value) -> Fut,
        Fut: Future<Output = Result<(), ChatGPTError>>,
    {
        validate_record(record)?;
        self.0
            .refresh(record, &CancellationToken::new(), persist)
            .await
    }
}

pub fn validate_connection(record: &Value) -> Result<(), ChatGPTError> {
    validate_record(record)
}
pub fn sharing_enabled(record: &Value) -> bool {
    record["scopes"]
        .as_array()
        .is_some_and(|s| s.iter().any(|s| s == crate::SHARING_SCOPE))
}
fn invalid() -> ChatGPTError {
    ChatGPTError::new(
        "invalid_authorization",
        "ChatGPT 授权回调或身份无效，请重新授权",
        false,
    )
}
fn callback_values(
    request: &Authorization,
    callback: &str,
) -> Result<(String, String), ChatGPTError> {
    let url = Url::parse(callback).map_err(|_| invalid())?;
    let expected = Url::parse(REDIRECT_URI).map_err(|_| invalid())?;
    if url.origin() != expected.origin()
        || url.path() != expected.path()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(invalid());
    }
    let pairs: Vec<_> = url.query_pairs().collect();
    let one = |key: &str| -> Result<Option<String>, ChatGPTError> {
        let values: Vec<_> = pairs.iter().filter(|(k, _)| k == key).collect();
        if values.len() > 1 {
            return Err(invalid());
        }
        Ok(values.first().map(|(_, v)| v.to_string()))
    };
    let state = one("state")?.ok_or_else(invalid)?;
    if state.len() != request.state.len()
        || state.as_bytes().ct_eq(request.state.as_bytes()).unwrap_u8() != 1
        || one("error")?.is_some()
    {
        return Err(invalid());
    }
    let code = one("code")?.filter(|s| !s.is_empty()).ok_or_else(invalid)?;
    let client = one("client_id")?
        .or_else(|| request.client_id.clone())
        .ok_or_else(invalid)?;
    if !crate::storage::identifier(&client, 200, b"_-")
        || client == "dynamic_agent_client"
        || request.client_id.as_ref().is_some_and(|s| s != &client)
    {
        return Err(invalid());
    }
    Ok((code, client))
}

/// 与本地 SDK 相同的刷新终止条件，宿主据此要求重新授权
pub fn refresh_requires_reauthentication(error: &ChatGPTError) -> bool {
    crate::errors::requires_reauthentication(error)
        || matches!(
            error.code.as_str(),
            "invalid_id_token" | "invalid_token_response" | "account_mismatch"
        )
}
