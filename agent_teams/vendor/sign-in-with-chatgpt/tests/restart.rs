//! 新进程在 ID token 过期后按加密 receipt-time 恢复轮换，不重复提交旧刷新令牌
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use super::support::*;
use crate::{
    storage::{Store, now},
    *,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::sync::Arc;

#[tokio::test]
async fn new_process_recovers_at_receipt_time_after_id_token_expiry() {
    if let Some(directory) = std::env::var_os("SIWC_SYNTHETIC_RECOVERY_DIRECTORY") {
        let mut encryption = Encryption::new();
        encryption.key = STANDARD
            .decode(std::env::var("SIWC_SYNTHETIC_RECOVERY_KEY").unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let provider = Arc::new(encryption);
        let keys: Value =
            serde_json::from_str(&std::env::var("SIWC_SYNTHETIC_RECOVERY_JWKS").unwrap()).unwrap();
        let mock = Mock::new(move |r| {
            let keys = keys.clone();
            async move {
                Ok(match r.url.as_str() {
                    "https://auth.openai.com/.well-known/openid-configuration" => discovery(),
                    "https://auth.openai.com/jwks" => json_response(200, keys),
                    "https://api.openai.com/v1/models" => {
                        assert_eq!(r.headers["authorization"], "Bearer successor-test-access");
                        json_response(200, json!({"models":[]}))
                    }
                    _ => panic!("restart must not submit a second refresh grant"),
                })
            }
        });
        let config = ChatGPTConfig {
            app_name: "Recovery Test".into(),
            app_id: "recovery-test".into(),
            redirect_port: 0,
            storage_dir: Some(directory.clone().into()),
            credential_encryption: provider.clone(),
            open_browser: None,
            send_host_id: false,
        };
        let client = ChatGPT::with_transport(config, mock).unwrap();
        assert_eq!(
            client.get_session().await.error.unwrap().code,
            "identity_verification_unavailable"
        );
        client.list_models(CancellationToken::new()).await.unwrap();
        assert!(client.get_session().await.error.is_none());
        let store = Store::new(directory.into(), provider).unwrap();
        let saved = store
            .lock(&CancellationToken::new())
            .await
            .unwrap()
            .read()
            .await
            .unwrap();
        assert!(saved["profiles"][0].get("pendingRefresh").is_none());
        return;
    }
    let f = Fixture::new(offline());
    let mut state = saved_state();
    state["profiles"][0]["pendingRefresh"] = json!({"credentials":{"accessToken":"successor-test-access","refreshToken":"successor-test-refresh","expiresAt":now()+3600000},"scopes":state["profiles"][0]["scopes"],"idToken":identity(json!({"iat":now()/1000-120,"exp":now()/1000-60})),"receivedAt":now()-120000});
    f.write(&state).await;
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::restart::new_process_recovers_at_receipt_time_after_id_token_expiry",
            "--nocapture",
        ])
        .env("SIWC_SYNTHETIC_RECOVERY_DIRECTORY", f.directory.path())
        .env(
            "SIWC_SYNTHETIC_RECOVERY_KEY",
            STANDARD.encode(f.provider.key),
        )
        .env("SIWC_SYNTHETIC_RECOVERY_JWKS", jwks().to_string())
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert!(
        f.read().await["profiles"][0]
            .get("pendingRefresh")
            .is_none()
    );
}
