//! 宿主管理 OAuth 的回调绑定、动态客户端交换和 SDK 身份验证回归

use super::support::*;
use crate::hosted::{API_BASE_URL, Authorization, OAuthClient, REDIRECT_URI, sharing_enabled};
use serde_json::json;

fn authorization() -> Authorization {
    Authorization {
        state: "synthetic-state".into(),
        nonce: "synthetic-nonce".into(),
        verifier: "synthetic-verifier".into(),
        host_id: "urn:uuid:00000000-0000-4000-8000-000000000001".into(),
        client_id: None,
    }
}
fn callback() -> String {
    format!(
        "{REDIRECT_URI}?state=synthetic-state&code=synthetic-code&client_id=registered_test_client"
    )
}

#[tokio::test]
async fn dynamic_registration_exchanges_issued_client_and_verifies_identity() {
    let transport = Mock::new(|request| async move {
        if request.url.ends_with("openid-configuration") {
            return Ok(discovery());
        }
        if request.url.ends_with("jwks") {
            return Ok(json_response(200, jwks()));
        }
        assert_eq!(form(&request)["client_id"], "registered_test_client");
        assert_eq!(form(&request)["resource"], API_BASE_URL);
        assert_eq!(form(&request)["redirect_uri"], REDIRECT_URI);
        let mut token = token_response(Some(identity(json!({"nonce":"synthetic-nonce"}))));
        token["scope"] = "openid offline_access chatgpt.tokens.use.direct".into();
        Ok(json_response(200, token))
    });
    let client = OAuthClient::new(transport);
    let request = authorization();
    let url =
        url::Url::parse(&OAuthClient::authorization_url(&request, "AICodexAgentTeams").unwrap())
            .unwrap();
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
    assert_eq!(query["client_id"], "dynamic_agent_client");
    assert_eq!(query["ext_agent_host_id"], request.host_id);
    assert_eq!(query["agent_name_hint"], "AICodexAgentTeams");
    let connection = client.complete(&request, &callback(), None).await.unwrap();
    assert_eq!(connection["clientId"], "registered_test_client");
    assert!(sharing_enabled(&connection));
}

#[test]
fn authorization_links_need_no_transport_for_registration_or_reauthorization() {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use sha2::{Digest, Sha256};

    for client_id in [None, Some("existing-client".to_owned())] {
        let mut request = authorization();
        request.client_id = client_id.clone();
        let url = url::Url::parse(
            &OAuthClient::authorization_url(&request, "AICodexAgentTeams").unwrap(),
        )
        .unwrap();
        assert_eq!(
            url.origin().ascii_serialization(),
            "https://auth.openai.com"
        );
        assert_eq!(url.path(), "/api/accounts/authorize");
        let query: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            query["client_id"],
            client_id.as_deref().unwrap_or("dynamic_agent_client")
        );
        assert_eq!(query["redirect_uri"], REDIRECT_URI);
        assert_eq!(query["resource"], API_BASE_URL);
        assert_eq!(query["state"], request.state);
        assert_eq!(query["nonce"], request.nonce);
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(
            query["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(request.verifier.as_bytes()))
        );
        assert!(
            query["scope"]
                .split_whitespace()
                .any(|s| s == "chatgpt.tokens.use.direct")
        );
        assert_eq!(query["prompt"], "consent");
        if client_id.is_some() {
            assert!(!query.contains_key("agent_name_hint"));
        } else {
            assert_eq!(query["agent_name_hint"], "AICodexAgentTeams");
        }
    }
}

#[tokio::test]
async fn invalid_callbacks_never_exchange_tokens() {
    let transport = Mock::new(|_| async { panic!("invalid callbacks must not reach network") });
    let client = OAuthClient::new(transport.clone());
    for callback in [
        callback().replace("synthetic-state", "wrong-state"),
        format!("{}&state=synthetic-state", callback()),
        callback().replace("127.0.0.1", "example.invalid"),
        callback().replace("registered_test_client", "dynamic_agent_client"),
        format!("{}&client_id=other", callback()),
        callback().replace("/auth/callback", "/other"),
    ] {
        assert!(
            client
                .complete(&authorization(), &callback, None)
                .await
                .is_err()
        );
    }
    let mut request = authorization();
    request.client_id = Some("original_client".into());
    assert!(client.complete(&request, &callback(), None).await.is_err());
    assert!(transport.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn wrong_nonce_rejects_verified_signature_and_declined_scope_stays_disabled() {
    for nonce in ["wrong-nonce", "synthetic-nonce"] {
        let transport = Mock::new(move |request| async move {
            if request.url.ends_with("openid-configuration") {
                return Ok(discovery());
            }
            if request.url.ends_with("jwks") {
                return Ok(json_response(200, jwks()));
            }
            Ok(json_response(
                200,
                json!({"id_token":identity(json!({"nonce":nonce})),"scope":"openid profile email"}),
            ))
        });
        let result = OAuthClient::new(transport)
            .complete(&authorization(), &callback(), None)
            .await;
        if nonce == "wrong-nonce" {
            assert!(result.is_err());
        } else {
            let record = result.unwrap();
            assert!(!sharing_enabled(&record));
            assert!(record.get("credentials").is_none());
        }
    }
}

#[tokio::test]
async fn refresh_without_id_token_checkpoints_successor_and_recovers_without_second_exchange() {
    use std::sync::{Arc, Mutex};
    let checkpoint = Arc::new(Mutex::new(None));
    let observed = checkpoint.clone();
    let transport = Mock::new(|request| async move {
        if request.url.ends_with("openid-configuration") {
            return Ok(discovery());
        }
        assert_eq!(form(&request)["client_id"], "registered_test_client");
        assert_eq!(form(&request)["resource"], API_BASE_URL);
        assert!(form(&request).get("scope").is_none());
        Ok(json_response(200, token_response(None)))
    });
    let previous = saved_state()["profiles"][0].clone();
    let result = OAuthClient::new(transport)
        .refresh(&previous, move |value| {
            *observed.lock().unwrap() = Some(value);
            async { Ok(()) }
        })
        .await
        .unwrap();
    assert_eq!(
        result["credentials"]["refreshToken"],
        "successor-test-refresh"
    );
    let mut durable = previous;
    durable["pendingRefresh"] = checkpoint
        .lock()
        .unwrap()
        .clone()
        .expect("durable successor");
    let offline =
        Mock::new(|_| async { panic!("recovery must not spend predecessor token again") });
    let recovered = OAuthClient::new(offline)
        .refresh(&durable, |_| async {
            panic!("checkpoint already persisted")
        })
        .await
        .unwrap();
    assert_eq!(recovered["credentials"], result["credentials"]);
    assert!(recovered.get("pendingRefresh").is_none());
}

#[test]
fn terminal_refresh_errors_require_reauthorization_but_outages_do_not() {
    for code in [
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
        "invalid_id_token",
        "invalid_token_response",
        "account_mismatch",
    ] {
        assert!(crate::hosted::refresh_requires_reauthentication(
            &crate::ChatGPTError::new(code, "synthetic", false)
        ));
    }
    for code in [
        "network_error",
        "identity_verification_unavailable",
        "storage_unavailable",
        "invalid_client",
    ] {
        assert!(!crate::hosted::refresh_requires_reauthentication(
            &crate::ChatGPTError::new(code, "synthetic", true)
        ));
    }
}

#[tokio::test]
async fn jwks_outage_after_code_exchange_requires_a_new_authorization() {
    let transport = Mock::new(|request| async move {
        if request.url.ends_with("openid-configuration") {
            return Ok(discovery());
        }
        if request.url.ends_with("jwks") {
            return Err(crate::ChatGPTError::new(
                "network_error",
                "synthetic outage",
                true,
            ));
        }
        Ok(json_response(
            200,
            token_response(Some(identity(json!({"nonce":"synthetic-nonce"})))),
        ))
    });
    let error = OAuthClient::new(transport)
        .complete(&authorization(), &callback(), None)
        .await
        .unwrap_err();
    assert_eq!(error.code, "authorization_restart_required");
    assert!(!error.retryable);
}
