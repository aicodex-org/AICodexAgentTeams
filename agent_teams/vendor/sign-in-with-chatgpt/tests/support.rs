//! 临时认证加密、合成 RSA 身份与可观察测试传输，不使用真实账号秘密
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use crate::{
    storage::{Store, now, saved_at},
    transport::{HttpRequest, HttpResponse, Transport},
    *,
};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use futures_util::stream;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use rand::RngCore;
use rsa::{RsaPrivateKey, pkcs8::EncodePrivateKey, traits::PublicKeyParts};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
use tempfile::TempDir;

pub struct Encryption {
    pub key: [u8; 32],
    pub available: AtomicBool,
    pub fail_encrypt: AtomicBool,
    pub fail_decrypt: AtomicBool,
    pub id: String,
}
impl Encryption {
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        Self {
            key,
            available: AtomicBool::new(true),
            fail_encrypt: AtomicBool::new(false),
            fail_decrypt: AtomicBool::new(false),
            id: "test-aes-gcm".into(),
        }
    }
}
#[async_trait]
impl CredentialEncryption for Encryption {
    fn id(&self) -> &str {
        &self.id
    }
    async fn is_available(&self) -> bool {
        self.available.load(Ordering::SeqCst)
    }
    async fn encrypt(&self, text: &str) -> Result<Vec<u8>, ChatGPTError> {
        if self.fail_encrypt.load(Ordering::SeqCst) {
            return Err(private_error());
        }
        let mut nonce = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ciphertext = Aes256Gcm::new_from_slice(&self.key)
            .unwrap()
            .encrypt(Nonce::from_slice(&nonce), text.as_bytes())
            .map_err(|_| private_error())?;
        Ok([nonce.to_vec(), ciphertext].concat())
    }
    async fn decrypt(&self, bytes: &[u8]) -> Result<String, ChatGPTError> {
        if self.fail_decrypt.load(Ordering::SeqCst) || bytes.len() < 28 {
            return Err(private_error());
        }
        let plain = Aes256Gcm::new_from_slice(&self.key)
            .unwrap()
            .decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..])
            .map_err(|_| private_error())?;
        String::from_utf8(plain).map_err(|_| private_error())
    }
}
pub fn private_error() -> ChatGPTError {
    ChatGPTError::new("test_error", "private-provider-error", false)
}
type MockFuture = Pin<Box<dyn Future<Output = Result<HttpResponse, ChatGPTError>> + Send>>;
pub struct Mock {
    handler: Arc<dyn Fn(HttpRequest) -> MockFuture + Send + Sync>,
    pub calls: Mutex<Vec<(String, String)>>,
}
impl Mock {
    pub fn new<F, Fut>(handler: F) -> Arc<Self>
    where
        F: Fn(HttpRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<HttpResponse, ChatGPTError>> + Send + 'static,
    {
        Arc::new(Self {
            handler: Arc::new(move |r| Box::pin(handler(r))),
            calls: Mutex::new(Vec::new()),
        })
    }
}
#[async_trait]
impl Transport for Mock {
    async fn send(&self, r: HttpRequest) -> Result<HttpResponse, ChatGPTError> {
        self.calls
            .lock()
            .unwrap()
            .push((r.method.into(), r.url.clone()));
        (self.handler)(r).await
    }
}
pub fn response(status: u16, body: Vec<u8>, headers: &[(&str, &str)]) -> HttpResponse {
    HttpResponse {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: Box::pin(stream::iter(vec![Ok(Bytes::from(body))])),
    }
}
pub fn json_response(status: u16, body: Value) -> HttpResponse {
    response(
        status,
        serde_json::to_vec(&body).unwrap(),
        &[
            ("content-type", "application/json"),
            ("x-request-id", "req_test"),
        ],
    )
}
pub fn discovery() -> HttpResponse {
    json_response(
        200,
        json!({"issuer":"https://auth.openai.com","authorization_endpoint":"https://auth.openai.com/authorize","token_endpoint":"https://auth.openai.com/oauth/token","jwks_uri":"https://auth.openai.com/jwks","revocation_endpoint":"https://auth.openai.com/revoke"}),
    )
}
pub fn form(r: &HttpRequest) -> Value {
    let mut v = json!({});
    for (k, val) in url::form_urlencoded::parse(&r.body) {
        v[k.as_ref()] = val.into_owned().into();
    }
    v
}
pub fn saved_state() -> Value {
    json!({"version":2,"activeProfileId":"profile-1","pendingRegistrations":[],"profiles":[{"version":1,"id":"profile-1","label":"Private profile label","clientId":"registered_test_client","subject":"private-subject","status":"connected","scopes":["openid","offline_access","chatgpt.tokens.use.direct"],"savedAt":saved_at(),"identity":{"name":"Test Person","email":"private@example.invalid"},"profileIdToken":"private-id-token","credentials":{"accessToken":"previous-test-access","refreshToken":"previous-test-refresh","expiresAt":now()+30000}}]})
}
pub struct Fixture {
    pub directory: TempDir,
    pub provider: Arc<Encryption>,
    pub store: Store,
    pub client: ChatGPT,
}
impl Fixture {
    pub fn new(transport: Arc<dyn Transport>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let provider = Arc::new(Encryption::new());
        let config = Self::config(&directory, provider.clone());
        let store = Store::new(directory.path().to_path_buf(), provider.clone()).unwrap();
        let client = ChatGPT::with_transport(config, transport).unwrap();
        Self {
            directory,
            provider,
            store,
            client,
        }
    }
    pub fn config(directory: &TempDir, provider: Arc<Encryption>) -> ChatGPTConfig {
        ChatGPTConfig {
            app_name: "Storage Test".into(),
            app_id: "storage-test".into(),
            redirect_port: 0,
            storage_dir: Some(directory.path().to_path_buf()),
            credential_encryption: provider,
            open_browser: None,
            send_host_id: false,
        }
    }
    pub async fn write(&self, state: &Value) {
        self.store
            .lock(&CancellationToken::new())
            .await
            .unwrap()
            .write(state)
            .await
            .unwrap();
    }
    pub async fn read(&self) -> Value {
        self.store
            .lock(&CancellationToken::new())
            .await
            .unwrap()
            .read()
            .await
            .unwrap()
    }
    pub fn raw(&self) -> Vec<u8> {
        std::fs::read(self.directory.path().join("chatgpt-auth.json")).unwrap()
    }
    pub fn write_raw(&self, bytes: &[u8]) {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(self.directory.path().join("chatgpt-auth.json"))
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
}
struct Keys {
    key: EncodingKey,
    jwk: Value,
}
fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(||{let key=RsaPrivateKey::new(&mut rand::rngs::OsRng,2048).unwrap();let jwk=json!({"kty":"RSA","kid":"test-key","alg":"RS256","use":"sig","n":URL_SAFE_NO_PAD.encode(key.n().to_bytes_be()),"e":URL_SAFE_NO_PAD.encode(key.e().to_bytes_be())});let pem=key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();Keys {key:EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),jwk}})
}
pub fn jwks() -> Value {
    json!({"keys":[keys().jwk]})
}
pub fn identity(overrides: Value) -> String {
    let mut claims = json!({"iss":"https://auth.openai.com","aud":"registered_test_client","sub":"private-subject","iat":now()/1000,"exp":now()/1000+3600});
    claims
        .as_object_mut()
        .unwrap()
        .extend(overrides.as_object().unwrap().clone());
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    jsonwebtoken::encode(&header, &claims, &keys().key).unwrap()
}
pub fn token_response(token: Option<String>) -> Value {
    let mut v = json!({"access_token":"successor-test-access","refresh_token":"successor-test-refresh","token_type":"Bearer","expires_in":3600});
    if let Some(token) = token {
        v["id_token"] = token.into();
    }
    v
}
pub fn identity_with_header(claims: Value, extra: Value) -> String {
    let token = identity(claims);
    let payload = token.split('.').nth(1).unwrap();
    let mut header = json!({"alg":"RS256","kid":"test-key","typ":"JWT"});
    header
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let message = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        payload
    );
    let signature =
        jsonwebtoken::crypto::sign(message.as_bytes(), &keys().key, Algorithm::RS256).unwrap();
    format!("{message}.{signature}")
}
pub fn offline() -> Arc<Mock> {
    Mock::new(|_| async { Err(crate::errors::network_error()) })
}
