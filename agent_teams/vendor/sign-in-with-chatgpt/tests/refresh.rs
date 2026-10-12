//! 官方令牌轮换、JWKS 故障恢复、重启与取消后的持久化用例
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use super::support::*;
use crate::{storage::now, *};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn provider(token: Value) -> Arc<Mock> {
    Mock::new(move |r| {
        let token = token.clone();
        async move {
            Ok(match r.url.as_str() {
                "https://auth.openai.com/.well-known/openid-configuration" => discovery(),
                "https://auth.openai.com/jwks" => json_response(200, jwks()),
                "https://auth.openai.com/oauth/token" => {
                    assert_eq!(form(&r)["grant_type"], "refresh_token");
                    json_response(200, token)
                }
                "https://api.openai.com/v1/models" => {
                    assert_eq!(r.headers["authorization"], "Bearer successor-test-access");
                    json_response(
                        200,
                        json!({"models":[{"slug":"test-model","display_name":"Test Model","visibility":"list"}]}),
                    )
                }
                _ => panic!("unexpected synthetic endpoint"),
            })
        }
    })
}

#[tokio::test]
async fn jwks_outages_preserve_checkpoint_and_recover_with_a_fresh_client() {
    let failure = Arc::new(AtomicUsize::new(0));
    let mode = failure.clone();
    let token = identity(json!({}));
    let mock = Mock::new(move |r| {
        let token = token.clone();
        let mode = mode.clone();
        async move {
            Ok(match r.url.as_str() {
                "https://auth.openai.com/.well-known/openid-configuration" => discovery(),
                "https://auth.openai.com/oauth/token" => {
                    json_response(200, token_response(Some(token)))
                }
                "https://auth.openai.com/jwks" => match mode.load(Ordering::SeqCst) {
                    0 => return Err(crate::errors::network_error()),
                    1 => response(503, b"Unavailable".to_vec(), &[]),
                    2 => return Err(crate::errors::cancelled()),
                    3 => response(200, b"{bad jwks".to_vec(), &[]),
                    4 => json_response(200, json!({"keys":[]})),
                    5 => {
                        let mut keys = jwks();
                        keys["keys"][0]["kid"] = "another-key".into();
                        json_response(200, keys)
                    }
                    6 => json_response(
                        200,
                        json!({"keys":[{"kty":"RSA","kid":"test-key","alg":"RS256"}]}),
                    ),
                    _ => json_response(200, jwks()),
                },
                "https://api.openai.com/v1/models" => {
                    assert_eq!(r.headers["authorization"], "Bearer successor-test-access");
                    json_response(200, json!({"models":[]}))
                }
                _ => panic!("unexpected synthetic endpoint"),
            })
        }
    });
    let f = Fixture::new(mock.clone());
    f.write(&saved_state()).await;
    for mode in 0..7 {
        failure.store(mode, Ordering::SeqCst);
        let e = f
            .client
            .list_models(CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(e.code, "identity_verification_unavailable");
        assert!(e.retryable);
        let p = f.read().await["profiles"][0].clone();
        assert_eq!(p["status"], "connected");
        assert_eq!(
            p["pendingRefresh"]["credentials"]["refreshToken"],
            "successor-test-refresh"
        );
        assert_eq!(p["credentials"]["refreshToken"], "previous-test-refresh");
        assert!(p["pendingRefresh"]["receivedAt"].is_i64());
        assert!(
            !String::from_utf8(f.raw())
                .unwrap()
                .contains("successor-test")
        );
        assert_eq!(
            f.client.get_session().await.error.unwrap().code,
            "identity_verification_unavailable"
        );
        assert!(
            !serde_json::to_string(&f.client.list_profiles().await.unwrap())
                .unwrap()
                .contains("successor-test")
        );
    }
    assert_eq!(
        mock.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, u)| u.ends_with("/oauth/token"))
            .count(),
        1
    );
    assert_eq!(
        mock.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, u)| u.ends_with("/jwks"))
            .count(),
        7
    );
    failure.store(7, Ordering::SeqCst);
    let restarted = ChatGPT::with_transport(
        Fixture::config(&f.directory, f.provider.clone()),
        mock.clone(),
    )
    .unwrap();
    assert!(restarted.get_session().await.error.is_some());
    restarted
        .list_models(CancellationToken::new())
        .await
        .unwrap();
    assert!(restarted.get_session().await.error.is_none());
    assert!(
        f.read().await["profiles"][0]
            .get("pendingRefresh")
            .is_none()
    );
    assert_eq!(
        mock.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, u)| u.ends_with("/oauth/token"))
            .count(),
        1
    );
}

#[tokio::test]
async fn refreshed_invalid_identities_clear_all_credentials() {
    for invalid in ["signature", "issuer", "audience", "subject", "expiry"] {
        let overrides = match invalid {
            "issuer" => json!({"iss":"https://invalid.example"}),
            "audience" => json!({"aud":"another-client"}),
            "subject" => json!({"sub":"another-account"}),
            "expiry" => json!({"exp":now()/1000-60}),
            _ => json!({}),
        };
        let mut token = identity(overrides);
        if invalid == "signature" {
            use base64::Engine;
            let parts: Vec<_> = token.split('.').collect();
            let mut signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(parts[2])
                .unwrap();
            signature[0] ^= 1;
            token = format!(
                "{}.{}.{}",
                parts[0],
                parts[1],
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature)
            );
        }
        let mock = provider(token_response(Some(token)));
        let f = Fixture::new(mock.clone());
        f.write(&saved_state()).await;
        assert_eq!(
            f.client
                .list_models(CancellationToken::new())
                .await
                .unwrap_err()
                .code,
            if invalid == "subject" {
                "account_mismatch"
            } else {
                "invalid_id_token"
            }
        );
        let p = f.read().await["profiles"][0].clone();
        assert_eq!(p["status"], "reauth_required");
        for key in ["credentials", "pendingRefresh", "profileIdToken"] {
            assert!(p.get(key).is_none());
        }
        assert_eq!(
            mock.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, u)| u.ends_with("/models"))
                .count(),
            0
        );
    }
}
#[tokio::test]
async fn refresh_without_id_token_persists_successor() {
    let mock = provider(token_response(None));
    let f = Fixture::new(mock.clone());
    f.write(&saved_state()).await;
    assert_eq!(
        f.client
            .list_models(CancellationToken::new())
            .await
            .unwrap()[0]
            .slug,
        "test-model"
    );
    assert_eq!(
        f.read().await["profiles"][0]["credentials"]["refreshToken"],
        "successor-test-refresh"
    );
    assert!(
        !mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, u)| u.ends_with("/jwks"))
    );
}
#[tokio::test]
async fn disconnect_revokes_pending_successor_and_removes_every_token() {
    let mock = Mock::new(|r| async move {
        Ok(match r.url.as_str() {
            "https://auth.openai.com/.well-known/openid-configuration" => discovery(),
            "https://auth.openai.com/revoke" => {
                assert_eq!(form(&r)["token"], "successor-test-refresh");
                response(200, vec![], &[])
            }
            _ => panic!("must not refresh or list models"),
        })
    });
    let f = Fixture::new(mock.clone());
    let mut state = saved_state();
    state["profiles"][0]["pendingRefresh"] = json!({"credentials":{"accessToken":"successor-test-access","refreshToken":"successor-test-refresh","expiresAt":now()+3600000},"scopes":state["profiles"][0]["scopes"],"idToken":identity(json!({})),"receivedAt":now()});
    f.write(&state).await;
    f.client.disconnect().await.unwrap();
    let p = f.read().await["profiles"][0].clone();
    assert_eq!(p["status"], "disconnected");
    assert!(p.get("credentials").is_none());
    assert!(p.get("pendingRefresh").is_none());
    assert_eq!(
        mock.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, u)| u.ends_with("/revoke"))
            .count(),
        1
    );
}
#[tokio::test]
async fn cancelled_token_bodies_do_not_rewrite_credentials() {
    for _name in ["AbortError", "TimeoutError"] {
        let mock = Mock::new(|r| async move {
            if r.url.ends_with("openid-configuration") {
                return Ok(discovery());
            }
            assert!(r.url.ends_with("/oauth/token"));
            let mut response = response(200, vec![], &[]);
            response.body = Box::pin(futures_util::stream::iter(vec![Err(
                crate::errors::cancelled(),
            )]));
            Ok(response)
        });
        let f = Fixture::new(mock);
        f.write(&saved_state()).await;
        let before = f.raw();
        assert_eq!(
            f.client
                .list_models(CancellationToken::new())
                .await
                .unwrap_err()
                .code,
            "cancelled"
        );
        assert_eq!(f.raw(), before);
    }
}
#[tokio::test]
async fn expired_pending_access_is_never_sent_and_only_verified_successor_is_renewed() {
    let mock = Mock::new(|r| async move {
        Ok(match r.url.as_str() {
            "https://auth.openai.com/.well-known/openid-configuration" => discovery(),
            "https://auth.openai.com/jwks" => json_response(200, jwks()),
            "https://auth.openai.com/oauth/token" => {
                assert_eq!(form(&r)["refresh_token"], "successor-test-refresh");
                let mut t = token_response(None);
                t["access_token"] = "renewed-test-access".into();
                t["refresh_token"] = "renewed-test-refresh".into();
                json_response(200, t)
            }
            "https://api.openai.com/v1/models" => {
                assert_eq!(r.headers["authorization"], "Bearer renewed-test-access");
                json_response(200, json!({"models":[]}))
            }
            _ => panic!("unexpected synthetic endpoint"),
        })
    });
    let f = Fixture::new(mock.clone());
    let mut state = saved_state();
    state["profiles"][0]["pendingRefresh"] = json!({"credentials":{"accessToken":"expired-pending-access","refreshToken":"successor-test-refresh","expiresAt":now()-30000},"scopes":state["profiles"][0]["scopes"],"idToken":identity(json!({"iat":now()/1000-120,"exp":now()/1000-60})),"receivedAt":now()-120000});
    f.write(&state).await;
    assert_eq!(
        f.client
            .list_models(CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "refresh_not_ready"
    );
    assert!(
        !mock
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(_, u)| u.ends_with("/oauth/token") || u.ends_with("/models"))
    );
    let p = f.read().await["profiles"][0].clone();
    assert!(p.get("pendingRefresh").is_none());
    assert_eq!(p["credentials"]["refreshToken"], "successor-test-refresh");
    f.client
        .list_models(CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        f.read().await["profiles"][0]["credentials"]["refreshToken"],
        "renewed-test-refresh"
    );
}
#[tokio::test]
async fn invalid_or_future_receipt_timestamps_are_rejected_before_persistence() {
    let f = Fixture::new(offline());
    f.write(&saved_state()).await;
    let before = f.raw();
    for received in [
        Value::Null,
        json!("yesterday"),
        json!(-1),
        json!(now() + 3600000),
    ] {
        let mut state = saved_state();
        state["profiles"][0]["pendingRefresh"] = json!({"credentials":{"accessToken":"pending","refreshToken":"pending-refresh","expiresAt":now()+3600000},"scopes":[],"idToken":identity(json!({})),"receivedAt":received});
        assert_eq!(
            f.store
                .lock(&CancellationToken::new())
                .await
                .unwrap()
                .write(&state)
                .await
                .unwrap_err()
                .code,
            "storage_invalid"
        );
        assert_eq!(f.raw(), before);
    }
}
#[tokio::test]
async fn caller_cancellation_does_not_discard_rotation() {
    let received = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let first = received.clone();
    let gate = release.clone();
    let mock = Mock::new(move |r| {
        let first = first.clone();
        let gate = gate.clone();
        async move {
            if r.url.ends_with("openid-configuration") {
                return Ok(discovery());
            }
            assert!(r.url.ends_with("/oauth/token"));
            first.notify_one();
            gate.notified().await;
            Ok(json_response(200, token_response(None)))
        }
    });
    let f = Fixture::new(mock);
    f.write(&saved_state()).await;
    let client = f.client.clone();
    let signal = CancellationToken::new();
    let caller = signal.clone();
    let task = tokio::spawn(async move { client.list_models(caller).await });
    received.notified().await;
    signal.cancel();
    assert_eq!(task.await.unwrap().unwrap_err().code, "cancelled");
    release.notify_one();
    let saved = f.read().await;
    assert_eq!(
        saved["profiles"][0]["credentials"]["refreshToken"],
        "successor-test-refresh"
    );
}
