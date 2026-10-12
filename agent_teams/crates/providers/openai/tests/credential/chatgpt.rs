//! ChatGPT 与 Codex 凭据隔离、授权范围及公开账号投影测试

use gateway_core::account::CredentialState;
use provider_openai::credential::{
    CodexCredentialCodec, CodexCredentialData, CodexRuntimeAuthentication,
    chatgpt::{AUTHENTICATION_KIND, ChatGptCredentialData},
};
use serde_json::json;

pub(crate) fn data(sharing: bool) -> ChatGptCredentialData {
    ChatGptCredentialData {
        schema_version: 1,
        installation_id: "00000000-0000-4000-8000-000000000001".into(),
        chatgpt_connection: json!({"version":1,"clientId":"issued-client","subject":"test-subject","status":"connected","savedAt":"2026-10-12T00:00:00Z","scopes":if sharing { vec!["openid", "chatgpt.tokens.use.direct"] } else { vec!["openid"] },"identity":{"email":"test@example.invalid"},"credentials":{"accessToken":"opaque-private-token","refreshToken":"private-refresh-token","expiresAt":1800000000000.0,"earliestRefreshAt":1799999990}}),
    }
}

#[test]
fn opaque_chatgpt_tokens_round_trip_without_codex_principal_or_cookies() {
    let data = data(true);
    assert_eq!(
        data.refresh_after()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        1799999990
    );
    let prepared = data.prepare_create("ChatGPT test".into()).unwrap();
    assert_eq!(prepared.account.authentication_kind(), AUTHENTICATION_KIND);
    assert_eq!(prepared.account.credential_state(), CredentialState::Ready);
    let decoded = CodexCredentialCodec::decode(&prepared.credential).unwrap();
    assert!(matches!(
        decoded.authentication,
        CodexRuntimeAuthentication::ChatGpt(_)
    ));
    assert!(decoded.authentication.oauth().is_none());
    assert!(decoded.principal.is_none());
    assert!(decoded.cookies.is_empty());
    let complete = CodexCredentialCodec::decode_complete(&prepared.credential).unwrap();
    assert!(matches!(complete, CodexCredentialData::ChatGpt(_)));
    let debug = format!("{decoded:?} {complete:?}");
    assert!(!debug.contains("private-token"));
    assert!(!debug.contains("private-refresh-token"));
}

#[test]
fn identity_only_and_pending_rotation_accounts_are_not_ready() {
    assert_eq!(
        data(false)
            .prepare_create("Identity".into())
            .unwrap()
            .account
            .credential_state(),
        CredentialState::Unknown
    );
    let mut data = data(true);
    data.chatgpt_connection["pendingRefresh"] = json!({"credentials":data.chatgpt_connection["credentials"],"scopes":["openid"],"idToken":"synthetic-id","receivedAt":chrono::Utc::now().timestamp_millis()});
    assert!(!data.ready());
    assert_eq!(
        data.prepare_create("Pending".into())
            .unwrap()
            .account
            .credential_state(),
        CredentialState::Unknown
    );
}

#[test]
fn registration_placeholder_is_not_a_saved_credential() {
    let mut data = data(true);
    data.chatgpt_connection["clientId"] = "dynamic_agent_client".into();
    assert!(CodexCredentialCodec::encode_complete(CodexCredentialData::ChatGpt(data)).is_err());
}

#[tokio::test]
async fn interrupted_refresh_is_due_immediately_and_recovers_saved_successor() {
    use crate::support::MemoryAccountStore;
    use gateway_core::account::{ProviderAccountStore, ProviderRefreshQuery};
    use std::{
        sync::Arc,
        time::{Duration, SystemTime},
    };
    let store = Arc::new(MemoryAccountStore::default());
    let mut pending = data(true);
    let now = chrono::Utc::now().timestamp_millis();
    pending.chatgpt_connection["pendingRefresh"] = json!({
        "credentials":{"accessToken":"successor-access","refreshToken":"successor-refresh","expiresAt":now + 3600000},
        "scopes":["openid", "chatgpt.tokens.use.direct"], "receivedAt":now
    });
    let prepared = pending
        .prepare_create("Interrupted refresh".into())
        .unwrap();
    let account = prepared.account.clone();
    assert_eq!(account.upstream_user_id(), Some("chatgpt:test-subject"));
    assert!(account.access_token_expires_at().unwrap() <= SystemTime::now());
    assert!(account.next_refresh_at().is_none());
    store.create_account(prepared).await.unwrap();
    let due = store
        .list_refresh_candidates(ProviderRefreshQuery::new(
            account.provider().clone(),
            SystemTime::now() + Duration::from_secs(300),
            SystemTime::UNIX_EPOCH,
            SystemTime::now(),
            vec![],
            std::num::NonZeroU32::new(10).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    let recovered =
        provider_openai::credential::chatgpt::prepare_refresh(&store.repository(), &account)
            .await
            .unwrap();
    let parts = recovered.credential.into_parts();
    assert_eq!(
        parts.account_state.unwrap().credential_state,
        CredentialState::Ready
    );
    let data = CodexCredentialCodec::decode_complete(&parts.credential).unwrap();
    let CodexCredentialData::ChatGpt(data) = data else {
        panic!("ChatGPT credential")
    };
    assert!(data.ready());
    assert_eq!(
        data.chatgpt_connection["credentials"]["refreshToken"],
        "successor-refresh"
    );
}

#[test]
fn oauth_transport_honors_the_gateways_custom_ca_configuration() {
    const CASE: &str = "AICODEX_TEST_CHATGPT_CUSTOM_CA";
    if std::env::var_os(CASE).is_some() {
        assert!(provider_openai::credential::chatgpt::oauth_client(None).is_err());
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let certificate = directory.path().join("invalid-ca.pem");
    std::fs::write(&certificate, "not a CA certificate").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "credential::chatgpt::oauth_transport_honors_the_gateways_custom_ca_configuration",
        ])
        .env(CASE, "1")
        .env(
            provider_openai::transport::tls::CODEX_CA_CERT_ENV,
            certificate,
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
