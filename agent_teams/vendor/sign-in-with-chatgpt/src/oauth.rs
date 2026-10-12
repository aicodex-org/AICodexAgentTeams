//! 官方 OIDC、PKCE 回调、身份验证与刷新轮换，浏览器 URL 不含已存令牌
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use crate::{
    BrowserOpener, CancellationToken, ChatGPTConfig, ChatGPTError, SHARING_SCOPE,
    errors::{api_error, cancelled},
    storage::{identifier, now, saved_at},
    transport::{Transport, json_response, request, send},
};
use async_trait::async_trait;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use rand::RngCore;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{future::Future, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Mutex, OnceCell, oneshot},
};
use url::Url;

pub(crate) const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const SCRIPT: &str = "history.replaceState(null, \"\", \"/auth/complete\");";

pub(crate) fn unavailable() -> ChatGPTError {
    ChatGPTError::new(
        "identity_verification_unavailable",
        "ChatGPT identity verification is temporarily unavailable. Your connection has been preserved. Try again shortly.",
        true,
    )
}
fn invalid_identity() -> ChatGPTError {
    ChatGPTError::new(
        "invalid_id_token",
        "The ChatGPT identity could not be verified. Please sign in again.",
        false,
    )
}
pub(crate) fn revocation_failed() -> ChatGPTError {
    ChatGPTError::new(
        "revocation_failed",
        "Local credentials were removed, but remote disconnection could not be confirmed. Disconnect the app in ChatGPT Settings.",
        true,
    )
}
fn random_value() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) struct OAuth {
    transport: Arc<dyn Transport>,
    discovery: OnceCell<Value>,
    keys: Mutex<Option<(tokio::time::Instant, Value)>>,
}
impl OAuth {
    pub(crate) fn new(transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            discovery: OnceCell::new(),
            keys: Mutex::new(None),
        }
    }
    pub(crate) async fn discovery(&self) -> Result<&Value, ChatGPTError> {
        self.discovery
            .get_or_try_init(|| async {
                let signal = CancellationToken::new();
                let response = send(
                    self.transport.as_ref(),
                    request("GET", &format!("{ISSUER}/.well-known/openid-configuration")),
                    &signal,
                    Duration::from_secs(30),
                )
                .await?;
                let status = response.status;
                let data = json_response(response, &signal, Duration::from_secs(30)).await?;
                let invalid = || {
                    ChatGPTError::new(
                        "discovery_failed",
                        "ChatGPT sign-in configuration could not be verified.",
                        true,
                    )
                };
                if !(200..300).contains(&status) || data["issuer"] != ISSUER {
                    return Err(invalid());
                }
                for key in [
                    "authorization_endpoint",
                    "token_endpoint",
                    "jwks_uri",
                    "revocation_endpoint",
                ] {
                    if key == "revocation_endpoint" && data.get(key).is_none() {
                        continue;
                    }
                    let url = data[key]
                        .as_str()
                        .and_then(|s| Url::parse(s).ok())
                        .ok_or_else(invalid)?;
                    if url.origin().ascii_serialization() != ISSUER {
                        return Err(invalid());
                    }
                }
                Ok(data)
            })
            .await
    }
    async fn verification_key(
        &self,
        uri: &str,
        kid: Option<&str>,
    ) -> Result<DecodingKey, ChatGPTError> {
        let mut cached = self.keys.lock().await;
        if cached
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(600))
        {
            let signal = CancellationToken::new();
            let response = send(
                self.transport.as_ref(),
                request("GET", uri),
                &signal,
                Duration::from_secs(15),
            )
            .await
            .map_err(|_| unavailable())?;
            if response.status != 200 {
                return Err(unavailable());
            }
            let data = json_response(response, &signal, Duration::from_secs(15))
                .await
                .map_err(|_| unavailable())?;
            if !data["keys"].is_array() {
                return Err(unavailable());
            }
            *cached = Some((tokio::time::Instant::now(), data));
        }
        let keys = &cached.as_ref().expect("cached keys").1["keys"];
        let eligible: Vec<_> = keys
            .as_array()
            .expect("validated array")
            .iter()
            .filter(|k| {
                k["kty"] == "RSA"
                    && kid.is_none_or(|kid| k["kid"] == kid)
                    && k.get("use").is_none_or(|v| v == "sig")
                    && k.get("alg").is_none_or(|v| v == "RS256")
                    && k.get("key_ops").is_none_or(|v| {
                        v.as_array()
                            .is_some_and(|a| a.iter().any(|v| v == "verify"))
                    })
            })
            .collect();
        let key = if eligible.len() == 1 {
            eligible[0]["n"]
                .as_str()
                .zip(eligible[0]["e"].as_str())
                .and_then(|(n, e)| DecodingKey::from_rsa_components(n, e).ok())
        } else {
            None
        };
        if key.is_none() {
            *cached = None;
        }
        key.ok_or_else(unavailable)
    }
    pub(crate) async fn verify_identity(
        &self,
        token: &str,
        client_id: &str,
        nonce: Option<&str>,
        received_at: Option<i64>,
    ) -> Result<Value, ChatGPTError> {
        let discovery = self.discovery().await.map_err(|_| unavailable())?;
        let header = decode_header(token).map_err(|_| invalid_identity())?;
        if header.alg != Algorithm::RS256 {
            return Err(invalid_identity());
        }
        let protected: Value = token
            .split('.')
            .next()
            .and_then(|s| URL_SAFE_NO_PAD.decode(s).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or_else(invalid_identity)?;
        // JWT 不接受未识别的 critical 扩展或未编码 payload，不能让解码器忽略这些约束
        if protected.get("crit").is_some_and(|v| {
            v.as_array()
                .is_none_or(|a| a.len() != 1 || a[0] != "b64" || protected["b64"] != true)
        }) {
            return Err(invalid_identity());
        }
        let key = self
            .verification_key(
                discovery["jwks_uri"].as_str().expect("validated uri"),
                header.kid.as_deref(),
            )
            .await?;
        // jsonwebtoken 的系统时钟不可替换，先验签再按官方 receipt-time 合同核对 claims
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        validation.validate_aud = false;
        validation.required_spec_claims.clear();
        let claims = decode::<Value>(token, &key, &validation)
            .map_err(|_| invalid_identity())?
            .claims;
        let now = (received_at.unwrap_or_else(now) as f64 / 1000.0).floor();
        let valid_aud = claims["aud"].as_str() == Some(client_id)
            || claims["aud"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v == client_id));
        let valid = claims["iss"] == ISSUER
            && valid_aud
            && claims["exp"].as_f64().is_some_and(|exp| exp > now - 5.0)
            && claims["iat"].is_number()
            && claims
                .get("nbf")
                .is_none_or(|v| v.as_f64().is_some_and(|n| n <= now + 5.0))
            && claims["sub"].as_str().is_some_and(|s| !s.is_empty())
            && nonce.is_none_or(|n| claims["nonce"] == n)
            && claims.get("azp").is_none_or(|v| v == client_id)
            && !claims["aud"]
                .as_array()
                .is_some_and(|a| a.len() > 1 && claims["azp"] != client_id);
        if !valid {
            return Err(invalid_identity());
        }
        let mut identity = json!({});
        for field in ["name", "email"] {
            if claims[field].is_string() {
                identity[field] = claims[field].clone();
            }
        }
        Ok(json!({"subject":claims["sub"],"identity":identity}))
    }
    pub(crate) async fn token_request(
        &self,
        fields: &[(&str, &str)],
        signal: &CancellationToken,
    ) -> Result<Value, ChatGPTError> {
        let provider = self.discovery().await?;
        let mut req = request(
            "POST",
            provider["token_endpoint"]
                .as_str()
                .expect("validated endpoint"),
        );
        req.headers.insert(
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        );
        req.headers
            .insert("accept".into(), "application/json".into());
        req.body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields.iter().copied())
            .finish()
            .into_bytes();
        let response = send(
            self.transport.as_ref(),
            req,
            signal,
            Duration::from_secs(30),
        )
        .await?;
        let status = response.status;
        let request_id = response.headers.get("x-request-id").cloned();
        let data = json_response(response, signal, Duration::from_secs(30)).await?;
        if !(200..300).contains(&status) {
            return Err(api_error(Some(&data), Some(status), request_id.as_deref()));
        }
        if !data.is_object() {
            return Err(ChatGPTError::new(
                "invalid_token_response",
                "ChatGPT returned an invalid token response. Please sign in again.",
                false,
            ));
        }
        Ok(data)
    }
    pub(crate) async fn authorize<F, Fut>(
        &self,
        config: &ChatGPTConfig,
        previous: Option<&Value>,
        host_id: &str,
        signal: &CancellationToken,
        reconsent: bool,
        mut on_registration: F,
    ) -> Result<Value, ChatGPTError>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<(), ChatGPTError>>,
    {
        let state = random_value();
        let nonce = random_value();
        let verifier = random_value();
        let provider = self.discovery().await?;
        if signal.is_cancelled() {
            return Err(cancelled());
        }
        let saved_client = previous.and_then(|v| v["clientId"].as_str());
        let listener = CallbackListener::bind(
            config.redirect_port,
            state.clone(),
            saved_client.map(str::to_owned),
        )
        .await?;
        let mut authorization = Url::parse(
            provider["authorization_endpoint"]
                .as_str()
                .expect("validated endpoint"),
        )
        .expect("validated URL");
        authorization.set_query(None);
        {
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            let mut query = authorization.query_pairs_mut();
            query.extend_pairs([
                ("client_id", saved_client.unwrap_or("dynamic_agent_client")),
                ("response_type", "code"),
                ("redirect_uri", listener.redirect_uri.as_str()),
                ("scope", SCOPES),
                ("resource", RESOURCE),
                ("state", state.as_str()),
                ("nonce", nonce.as_str()),
                ("code_challenge_method", "S256"),
                ("code_challenge", challenge.as_str()),
            ]);
            if config.send_host_id {
                query.append_pair("ext_agent_host_id", host_id);
            }
            if saved_client.is_none() {
                query.append_pair("agent_name_hint", &config.app_name);
            }
            if let Some(email) = previous
                .and_then(|v| v["identity"]["email"].as_str())
                .filter(|s| !s.is_empty())
            {
                query.append_pair("login_hint", email);
            }
            if reconsent {
                query.append_pair("prompt", "consent");
            }
        }
        let opener = config.open_browser.as_deref().unwrap_or(&DefaultBrowser);
        tokio::select! { biased; _=signal.cancelled()=>return Err(cancelled()), result=opener.open(authorization.as_str())=>result? }
        let callback = tokio::select! { biased; _=signal.cancelled()=>return Err(cancelled()), result=listener.result()=>result? };
        on_registration(callback.client_id.clone()).await?;
        let data = self
            .token_request(
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", &callback.client_id),
                    ("code", &callback.code),
                    ("code_verifier", &verifier),
                    ("redirect_uri", &listener.redirect_uri),
                    ("resource", RESOURCE),
                ],
                signal,
            )
            .await?;
        let token = data["id_token"].as_str().ok_or_else(|| {
            ChatGPTError::new(
                "invalid_id_token",
                "ChatGPT did not return a verifiable identity. Please try signing in again.",
                false,
            )
        })?;
        let verified = self
            .verify_identity(token, &callback.client_id, Some(&nonce), None)
            .await?;
        if previous.is_some_and(|p| {
            p["subject"].as_str().is_some_and(|s| !s.is_empty())
                && p["subject"] != verified["subject"]
        }) {
            return Err(ChatGPTError::new(
                "account_mismatch",
                "This sign-in returned a different ChatGPT account. Choose the original account or add a separate profile.",
                false,
            ));
        }
        if signal.is_cancelled() {
            return Err(cancelled());
        }
        let mut result = json!({"version":1,"clientId":callback.client_id,"status":"connected","savedAt":saved_at(),"profileIdToken":token});
        merge(&mut result, &verified);
        merge(&mut result, &token_fields(&data, false)?);
        Ok(result)
    }
    pub(crate) async fn refresh<F, Fut>(
        &self,
        previous: &Value,
        signal: &CancellationToken,
        mut persist: F,
    ) -> Result<Value, ChatGPTError>
    where
        F: FnMut(Value) -> Fut,
        Fut: Future<Output = Result<(), ChatGPTError>>,
    {
        let mut rotation = previous.get("pendingRefresh").cloned();
        let fields = if let Some(pending) = &rotation {
            json!({"scopes":pending["scopes"],"credentials":pending["credentials"]})
        } else {
            let refresh = previous["credentials"]["refreshToken"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    ChatGPTError::new(
                        "invalid_grant",
                        "Your ChatGPT connection has expired. Sign in again.",
                        false,
                    )
                })?;
            let mut data = self
                .token_request(
                    &[
                        ("grant_type", "refresh_token"),
                        (
                            "client_id",
                            previous["clientId"].as_str().expect("validated client"),
                        ),
                        ("refresh_token", refresh),
                        ("resource", RESOURCE),
                    ],
                    signal,
                )
                .await?;
            if data.get("scope").is_none_or(Value::is_null) {
                data["scope"] = previous["scopes"]
                    .as_array()
                    .expect("validated scopes")
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .into();
            }
            let fields = token_fields(&data, true)?;
            if data
                .get("id_token")
                .is_some_and(|v| v.as_str().is_none_or(str::is_empty))
            {
                return Err(invalid_identity());
            }
            // 宿主可能在返回后中断，所有 successor 都先保存，再交付最终记录
            let mut checkpoint = json!({"credentials":fields["credentials"],"scopes":fields["scopes"],"receivedAt":now()});
            if let Some(id_token) = data["id_token"].as_str() {
                checkpoint["idToken"] = id_token.into();
            }
            persist(checkpoint.clone()).await?;
            rotation = Some(checkpoint);
            fields
        };
        let mut result = previous.clone();
        result
            .as_object_mut()
            .expect("validated profile")
            .remove("pendingRefresh");
        merge(&mut result, &fields);
        if let Some(rotation) = rotation
            && let Some(id_token) = rotation["idToken"].as_str()
        {
            let verified = self
                .verify_identity(
                    id_token,
                    previous["clientId"].as_str().expect("validated client"),
                    None,
                    rotation["receivedAt"].as_i64(),
                )
                .await?;
            if verified["subject"] != previous["subject"] {
                return Err(ChatGPTError::new(
                    "account_mismatch",
                    "The refreshed ChatGPT identity does not match this profile. Sign in to the original account again.",
                    false,
                ));
            }
            merge(&mut result, &verified);
            result["profileIdToken"] = rotation["idToken"].clone();
        }
        result["status"] = "connected".into();
        result["savedAt"] = saved_at().into();
        Ok(result)
    }
    pub(crate) async fn revoke(&self, connection: &Value) -> Result<(), ChatGPTError> {
        if connection.get("credentials").is_none() && connection.get("pendingRefresh").is_none() {
            return Ok(());
        }
        let token = connection["pendingRefresh"]["credentials"]["refreshToken"]
            .as_str()
            .or_else(|| connection["credentials"]["refreshToken"].as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(revocation_failed)?;
        let provider = self.discovery().await.map_err(|_| revocation_failed())?;
        let endpoint = provider["revocation_endpoint"]
            .as_str()
            .ok_or_else(revocation_failed)?;
        for attempt in 0..2 {
            let mut req = request("POST", endpoint);
            req.headers.insert(
                "content-type".into(),
                "application/x-www-form-urlencoded".into(),
            );
            req.body = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs([
                    ("token", token),
                    ("token_type_hint", "refresh_token"),
                    (
                        "client_id",
                        connection["clientId"].as_str().expect("validated client"),
                    ),
                ])
                .finish()
                .into_bytes();
            let response = send(
                self.transport.as_ref(),
                req,
                &CancellationToken::new(),
                Duration::from_secs(10),
            )
            .await;
            let retryable = match response {
                Ok(response) if response.status == 200 => return Ok(()),
                Ok(response) => response.status >= 500,
                Err(e) => e.code == "network_error",
            };
            if !retryable || attempt == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Err(revocation_failed())
    }
}

pub(crate) fn merge(target: &mut Value, source: &Value) {
    target
        .as_object_mut()
        .expect("object target")
        .extend(source.as_object().expect("object source").clone());
}
pub(crate) fn token_fields(data: &Value, require_refresh: bool) -> Result<Value, ChatGPTError> {
    let scope = data["scope"].as_str().ok_or_else(|| {
        ChatGPTError::new(
            "invalid_token_response",
            "ChatGPT did not confirm the granted permissions. Please sign in again.",
            false,
        )
    })?;
    let scopes: Vec<_> = scope.split_whitespace().collect();
    let mut result = json!({"scopes":scopes});
    if let Some(access) = data["access_token"].as_str().filter(|s| !s.is_empty()) {
        let expires = data["expires_in"]
            .as_f64()
            .filter(|n| *n > 0.0 && (now() as f64 + n * 1000.0).is_finite());
        let refresh = data["refresh_token"].as_str().filter(|s| !s.is_empty());
        if !data["token_type"]
            .as_str()
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
            || expires.is_none()
            || ((require_refresh || scopes.contains(&"offline_access")) && refresh.is_none())
        {
            return Err(ChatGPTError::new(
                "invalid_token_response",
                "ChatGPT returned incomplete credentials. Please sign in again.",
                false,
            ));
        }
        let mut credentials = json!({"accessToken":access,"expiresAt":now() as f64+expires.expect("validated expiry")*1000.0});
        if let Some(refresh) = refresh {
            credentials["refreshToken"] = refresh.into();
        }
        if data["earliest_refresh_at"].is_string() || data["earliest_refresh_at"].is_number() {
            credentials["earliestRefreshAt"] = data["earliest_refresh_at"].clone();
        }
        result["credentials"] = credentials;
    } else if require_refresh
        || scopes.contains(&"offline_access")
        || scopes.contains(&SHARING_SCOPE)
    {
        return Err(ChatGPTError::new(
            "invalid_token_response",
            "ChatGPT did not return the requested credentials. Please sign in again.",
            false,
        ));
    }
    Ok(result)
}

pub(crate) struct DefaultBrowser;
pub(crate) fn browser_command(url: &str) -> tokio::process::Command {
    let command = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "rundll32"
    } else {
        "xdg-open"
    };
    let mut child = tokio::process::Command::new(command);
    if cfg!(windows) {
        child.arg("url.dll,FileProtocolHandler");
    }
    child
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    child
}
#[async_trait]
impl BrowserOpener for DefaultBrowser {
    async fn open(&self, url: &str) -> Result<(), ChatGPTError> {
        let mut child = browser_command(url);
        let error = || {
            ChatGPTError::new(
                "browser_unavailable",
                "The browser could not be opened. Check your default browser and try again.",
                false,
            )
        };
        if child.status().await.map_err(|_| error())?.success() {
            Ok(())
        } else {
            Err(error())
        }
    }
}
struct Callback {
    code: String,
    client_id: String,
}
struct CallbackListener {
    redirect_uri: String,
    receiver: Mutex<Option<oneshot::Receiver<Result<Callback, ChatGPTError>>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for CallbackListener {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl CallbackListener {
    async fn bind(
        port: u16,
        state: String,
        saved_client: Option<String>,
    ) -> Result<Self, ChatGPTError> {
        let listener=TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,port)).await.map_err(|_|ChatGPTError::new("callback_port_unavailable","The sign-in port is unavailable. Close another copy of the app or choose a different redirect port.",false))?;
        let port = listener.local_addr().map_err(|_| cancelled())?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
        let (sender, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=>match accepted {
                        Ok((mut socket,_))=> {
                            let state=state.clone(); let saved_client=saved_client.clone();
                            connections.spawn(async move {
                                tokio::time::timeout(Duration::from_secs(15),async {
                                    let mut bytes=Vec::new(); let mut chunk=[0u8;1024];
                                    while !bytes.windows(4).any(|b|b==b"\r\n\r\n") {
                                        let n=socket.read(&mut chunk).await.ok()?; if n==0 { return None; }
                                        bytes.extend_from_slice(&chunk[..n]); if bytes.len()>16384 { return None; }
                                    }
                                    let text=std::str::from_utf8(&bytes).ok()?;
                                    let (status,body,result)=callback_request(text,port,&state,saved_client.as_deref());
                                    let csp=format!("default-src 'none'; script-src 'sha256-{}'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'",STANDARD.encode(Sha256::digest(SCRIPT.as_bytes())));
                                    let content_type=if status==200 { "text/html; charset=utf-8" } else { "text/plain" };
                                    let wire=format!("HTTP/1.1 {status} {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: {csp}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",if status==200 {"OK"} else if status==400 {"Bad Request"} else {"Not Found"},body.len());
                                    let _=socket.write_all(wire.as_bytes()).await; let _=socket.shutdown().await; result
                                }).await.ok().flatten()
                            });
                        },
                        Err(_)=> { let _=sender.send(Err(ChatGPTError::new("callback_failed","The local sign-in listener stopped. Please try again.",false))); break; }
                    },
                    completed=connections.join_next(), if !connections.is_empty()=> {
                        if let Some(Ok(Some(result)))=completed { let _=sender.send(result); break; }
                    }
                }
            }
        });
        Ok(Self {
            redirect_uri,
            receiver: Mutex::new(Some(receiver)),
            task,
        })
    }
    async fn result(&self) -> Result<Callback, ChatGPTError> {
        self.receiver
            .lock()
            .await
            .take()
            .expect("single callback consumer")
            .await
            .map_err(|_| {
                ChatGPTError::new(
                    "callback_failed",
                    "The local sign-in listener stopped. Please try again.",
                    false,
                )
            })?
    }
}
fn callback_request(
    text: &str,
    port: u16,
    state: &str,
    saved_client: Option<&str>,
) -> (u16, String, Option<Result<Callback, ChatGPTError>>) {
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split_whitespace();
    let method = first.next().unwrap_or_default();
    let target = first.next().unwrap_or("/");
    let hosts: Vec<_> = lines
        .filter_map(|line| line.split_once(':'))
        .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.trim())
        .collect();
    let origin = format!("http://127.0.0.1:{port}");
    let Ok(url) = Url::parse(&origin).and_then(|base| base.join(target)) else {
        return (400, "Invalid request".into(), None);
    };
    if method != "GET"
        || hosts != vec![format!("127.0.0.1:{port}").as_str()]
        || url.origin().ascii_serialization() != origin
        || url.path() != "/auth/callback"
    {
        return (404, "Not found".into(), None);
    }
    let params: Vec<_> = url.query_pairs().collect();
    let values = |key: &str| {
        params
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_ref())
            .collect::<Vec<_>>()
    };
    let states = values("state");
    if states.len() != 1
        || states[0].len() != state.len()
        || states[0].as_bytes().ct_eq(state.as_bytes()).unwrap_u8() != 1
    {
        return (
            400,
            "Invalid sign-in state. Return to the browser tab that started sign-in.".into(),
            None,
        );
    }
    let body = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><title>Return to your app</title><style>body{{font:17px system-ui;max-width:32rem;margin:18vh auto;padding:24px;color:#202123}}h1{{font-size:26px}}</style><h1>Return to your app</h1><p>The app will finish checking your ChatGPT connection. You can close this tab.</p><script>{SCRIPT}</script></html>"
    );
    if let Some(error) = values("error").first() {
        return (
            200,
            body,
            Some(Err(api_error(Some(&json!({"error":error})), None, None))),
        );
    }
    let codes = values("code");
    let clients = values("client_id");
    let client = clients.first().copied().or(saved_client);
    let valid = codes.len() == 1
        && !codes[0].is_empty()
        && clients.len() <= 1
        && client.is_some_and(|c| identifier(c, 200, b"_-") && c != "dynamic_agent_client")
        && !saved_client.is_some_and(|s| clients.first().is_some_and(|c| *c != s));
    let result = if valid {
        Ok(Callback {
            code: codes[0].into(),
            client_id: client.expect("validated client").into(),
        })
    } else {
        Err(ChatGPTError::new(
            "registration_incomplete",
            "ChatGPT did not complete app registration. Please try signing in again.",
            false,
        ))
    };
    (200, body, Some(result))
}
