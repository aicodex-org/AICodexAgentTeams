//! 官方重授权、回调绑定、动态注册复用与多账号状态机用例
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use super::support::*;
use crate::{oauth::OAuth, *};
use async_trait::async_trait;
use serde_json::json;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use url::Url;

type OpenFuture = Pin<Box<dyn Future<Output = Result<(), ChatGPTError>> + Send>>;
struct Opener(Arc<dyn Fn(String) -> OpenFuture + Send + Sync>);
impl Opener {
    fn new<F, Fut>(handler: F) -> Arc<Self>
    where
        F: Fn(String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), ChatGPTError>> + Send + 'static,
    {
        Arc::new(Self(Arc::new(move |u| Box::pin(handler(u)))))
    }
}
#[async_trait]
impl BrowserOpener for Opener {
    async fn open(&self, url: &str) -> Result<(), ChatGPTError> {
        (self.0)(url.into()).await
    }
}
async fn visit(url: &Url, client: Option<&str>) -> reqwest::Response {
    let params = url
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut callback = Url::parse(&params["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .append_pair("code", "synthetic-authorization-code")
        .append_pair("state", &params["state"]);
    if let Some(client) = client {
        callback.query_pairs_mut().append_pair("client_id", client);
    }
    reqwest::get(callback).await.unwrap()
}
#[tokio::test]
async fn reauthorization_keeps_saved_tokens_out_of_browser_urls_and_checks_identity() {
    for invalid in ["none", "subject", "nonce"] {
        let authorization = Arc::new(Mutex::new(None::<Url>));
        let observed = authorization.clone();
        let opener = Opener::new(move |url| {
            let observed = observed.clone();
            async move {
                for token in [
                    "private-id-token",
                    "previous-test-access",
                    "previous-test-refresh",
                ] {
                    assert!(!url.contains(token));
                }
                let url = Url::parse(&url).unwrap();
                let params = url
                    .query_pairs()
                    .collect::<std::collections::BTreeMap<_, _>>();
                assert_eq!(
                    url.origin().ascii_serialization(),
                    "https://auth.openai.com"
                );
                assert!(!params.contains_key("id_token_hint"));
                assert_eq!(params["client_id"], "registered_test_client");
                assert_eq!(params["login_hint"], "private@example.invalid");
                assert_eq!(params["prompt"], "consent");
                assert_eq!(params["response_type"], "code");
                assert_eq!(params["code_challenge_method"], "S256");
                assert!(!params.contains_key("ext_agent_host_id"));
                *observed.lock().unwrap() = Some(url.clone());
                assert_eq!(visit(&url, None).await.status(), 200);
                Ok(())
            }
        });
        let network = authorization.clone();
        let mock = Mock::new(move |r| {
            let network = network.clone();
            async move {
                Ok(match r.url.as_str() {
                    "https://auth.openai.com/.well-known/openid-configuration" => discovery(),
                    "https://auth.openai.com/jwks" => json_response(200, jwks()),
                    "https://auth.openai.com/oauth/token" => {
                        use base64::Engine;
                        use sha2::{Digest, Sha256};
                        let fields = form(&r);
                        let url = network.lock().unwrap().clone().unwrap();
                        let params = url
                            .query_pairs()
                            .collect::<std::collections::BTreeMap<_, _>>();
                        assert_eq!(fields["grant_type"], "authorization_code");
                        assert_eq!(fields["client_id"], "registered_test_client");
                        assert_eq!(fields["code"], "synthetic-authorization-code");
                        assert_eq!(
                            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                                Sha256::digest(
                                    fields["code_verifier"].as_str().unwrap().as_bytes()
                                )
                            ),
                            params["code_challenge"]
                        );
                        let claims = if invalid == "subject" {
                            json!({"nonce":params["nonce"],"sub":"different-subject"})
                        } else if invalid == "nonce" {
                            json!({"nonce":"unrelated-nonce"})
                        } else {
                            json!({"nonce":params["nonce"]})
                        };
                        let mut token = token_response(Some(identity(claims)));
                        token["scope"] = "openid offline_access".into();
                        json_response(200, token)
                    }
                    _ => panic!("unexpected synthetic endpoint"),
                })
            }
        });
        let f = Fixture::new(mock.clone());
        let mut config = Fixture::config(&f.directory, f.provider.clone());
        config.open_browser = Some(opener);
        let result = OAuth::new(mock.clone())
            .authorize(
                &config,
                Some(&saved_state()["profiles"][0]),
                "synthetic-host",
                &CancellationToken::new(),
                true,
                |_| async { Ok(()) },
            )
            .await;
        if invalid == "none" {
            assert_eq!(result.unwrap()["subject"], "private-subject");
        } else {
            assert_eq!(
                result.unwrap_err().code,
                if invalid == "subject" {
                    "account_mismatch"
                } else {
                    "invalid_id_token"
                }
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
    }
}
#[tokio::test]
async fn unrelated_callback_states_do_not_consume_transaction_and_security_headers_are_set() {
    let opener = Opener::new(|url| async move {
        let url = Url::parse(&url).unwrap();
        let params = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut callback = Url::parse(&params["redirect_uri"]).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", "wrong-state")
            .append_pair("code", "bad");
        assert_eq!(reqwest::get(callback).await.unwrap().status(), 400);
        let response = visit(&url, Some("dynamic_agent_client")).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(response.headers()["referrer-policy"], "no-referrer");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert!(
            response
                .text()
                .await
                .unwrap()
                .contains("history.replaceState")
        );
        Ok(())
    });
    let mock = Mock::new(|_| async { Ok(discovery()) });
    let f = Fixture::new(mock.clone());
    let mut config = Fixture::config(&f.directory, f.provider.clone());
    config.open_browser = Some(opener);
    assert_eq!(
        OAuth::new(mock)
            .authorize(
                &config,
                None,
                "host",
                &CancellationToken::new(),
                false,
                |_| async { Ok(()) }
            )
            .await
            .unwrap_err()
            .code,
        "registration_incomplete"
    );
}
#[tokio::test]
async fn invalid_grant_retries_using_durable_registration_then_multi_profile_state_is_safe() {
    let authorization = Arc::new(Mutex::new(None::<Url>));
    let observed = authorization.clone();
    let launches = Arc::new(AtomicUsize::new(0));
    let count = launches.clone();
    let opener = Opener::new(move |url| {
        let observed = observed.clone();
        let count = count.clone();
        async move {
            let url = Url::parse(&url).unwrap();
            let params = url
                .query_pairs()
                .collect::<std::collections::BTreeMap<_, _>>();
            let n = count.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                params["client_id"],
                if n == 0 || n == 2 {
                    "dynamic_agent_client"
                } else {
                    "registered_test_client"
                }
            );
            assert!(params.contains_key("ext_agent_host_id"));
            *observed.lock().unwrap() = Some(url.clone());
            assert_eq!(
                visit(&url, Some("registered_test_client")).await.status(),
                200
            );
            Ok(())
        }
    });
    let network = authorization.clone();
    let tokens = Arc::new(AtomicUsize::new(0));
    let count = tokens.clone();
    let mock = Mock::new(move |r| {
        let network = network.clone();
        let count = count.clone();
        async move {
            Ok(match r.url.as_str() {
                "https://auth.openai.com/.well-known/openid-configuration" => discovery(),
                "https://auth.openai.com/jwks" => json_response(200, jwks()),
                "https://auth.openai.com/oauth/token" => {
                    if count.fetch_add(1, Ordering::SeqCst) == 0 {
                        json_response(400, json!({"error":"invalid_grant"}))
                    } else {
                        let url = network.lock().unwrap().clone().unwrap();
                        let params = url
                            .query_pairs()
                            .collect::<std::collections::BTreeMap<_, _>>();
                        let mut token =
                            token_response(Some(identity(json!({"nonce":params["nonce"]}))));
                        token["scope"] = "openid offline_access chatgpt.tokens.use.direct".into();
                        json_response(200, token)
                    }
                }
                "https://auth.openai.com/revoke" => response(200, vec![], &[]),
                _ => panic!("unexpected synthetic endpoint"),
            })
        }
    });
    let f = Fixture::new(mock.clone());
    let mut config = Fixture::config(&f.directory, f.provider.clone());
    config.open_browser = Some(opener);
    config.send_host_id = true;
    let client = ChatGPT::with_transport(config, mock).unwrap();
    let states = Arc::new(Mutex::new(Vec::new()));
    let observed = states.clone();
    let subscription = client.subscribe(move |state| observed.lock().unwrap().push(state));
    assert_eq!(
        states.lock().unwrap()[0].status,
        SessionStatus::Disconnected
    );
    let first = client.sign_in(SignInOptions::default()).await.unwrap();
    assert!(first.sharing);
    assert_eq!(launches.load(Ordering::SeqCst), 2);
    assert!(
        f.read().await["pendingRegistrations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        states.lock().unwrap().last().unwrap().status,
        SessionStatus::Connected
    );
    assert!(
        states
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.status == SessionStatus::Connecting)
    );
    let second = client
        .sign_in(SignInOptions {
            new_profile: true,
            label: Some(first.profile_label.clone().unwrap()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_ne!(first.profile_id, second.profile_id);
    assert_eq!(second.profile_label, Some("Connection 1 (2)".into()));
    assert_eq!(client.list_profiles().await.unwrap().len(), 2);
    assert_eq!(
        client
            .select_profile(first.profile_id.as_deref().unwrap())
            .await
            .unwrap()
            .profile_id,
        first.profile_id
    );
    client.disconnect().await.unwrap();
    let p = client.list_profiles().await.unwrap();
    assert_eq!(p[0].status, SessionStatus::Disconnected);
    assert_eq!(p[1].status, SessionStatus::Connected);
    assert!(
        !serde_json::to_string(&p)
            .unwrap()
            .contains("successor-test")
    );
    drop(subscription);
    let observed = states.lock().unwrap().len();
    client.get_session().await;
    assert_eq!(states.lock().unwrap().len(), observed);
}

#[test]
fn default_browser_uses_direct_process_arguments_without_shell() {
    let url = "https://auth.openai.com/authorize?client_id=registered_test_client&login_hint=synthetic%40example.invalid";
    let command = crate::oauth::browser_command(url);
    let command = command.as_std();
    let expected = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "rundll32"
    } else {
        "xdg-open"
    };
    assert_eq!(command.get_program(), std::ffi::OsStr::new(expected));
    let args: Vec<_> = command.get_args().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(
        args,
        if cfg!(windows) {
            vec!["url.dll,FileProtocolHandler", url]
        } else {
            vec![url]
        }
    );
}

#[tokio::test]
async fn critical_headers_and_required_claims_follow_jose_rules() {
    let mock = Mock::new(|r| async move {
        Ok(if r.url.ends_with("openid-configuration") {
            discovery()
        } else {
            json_response(200, jwks())
        })
    });
    let oauth = OAuth::new(mock);
    for (extra, valid) in [
        (json!({}), true),
        (json!({"crit":["b64"],"b64":true}), true),
        (json!({"crit":["b64","b64"],"b64":true}), false),
        (json!({"crit":["unknown"],"unknown":true}), false),
        (json!({"crit":[]}), false),
        (json!({"crit":["b64"],"b64":false}), false),
        (json!({"crit":["b64"]}), false),
    ] {
        let result = oauth
            .verify_identity(
                &identity_with_header(json!({}), extra),
                "registered_test_client",
                None,
                None,
            )
            .await;
        if valid {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err().code, "invalid_id_token");
        }
    }
    for claims in [
        json!({"iat":null}),
        json!({"exp":null}),
        json!({"sub":""}),
        json!({"nbf":crate::storage::now()/1000+60}),
        json!({"aud":["registered_test_client","another-client"]}),
        json!({"azp":"another-client"}),
    ] {
        assert_eq!(
            oauth
                .verify_identity(&identity(claims), "registered_test_client", None, None)
                .await
                .unwrap_err()
                .code,
            "invalid_id_token"
        );
    }
}
#[tokio::test]
async fn cancel_signin_releases_callback_and_does_not_reset_host_identity() {
    let opened = Arc::new(tokio::sync::Notify::new());
    let notification = opened.clone();
    let redirect = Arc::new(Mutex::new(None));
    let observed = redirect.clone();
    let opener = Opener::new(move |url| {
        let notification = notification.clone();
        let observed = observed.clone();
        async move {
            let url = Url::parse(&url).unwrap();
            let params = url
                .query_pairs()
                .collect::<std::collections::BTreeMap<_, _>>();
            *observed.lock().unwrap() = Some(params["redirect_uri"].to_string());
            notification.notify_one();
            Ok(())
        }
    });
    let mock = Mock::new(|_| async { Ok(discovery()) });
    let f = Fixture::new(mock.clone());
    let mut config = Fixture::config(&f.directory, f.provider.clone());
    config.open_browser = Some(opener);
    let client = ChatGPT::with_transport(config, mock).unwrap();
    let running = client.clone();
    let task = tokio::spawn(async move { running.sign_in(SignInOptions::default()).await });
    opened.notified().await;
    assert_eq!(
        client.select_profile("profile-1").await.unwrap_err().code,
        "connection_busy"
    );
    client.cancel_sign_in();
    assert_eq!(task.await.unwrap().unwrap_err().code, "cancelled");
    assert_eq!(
        client.get_session().await.status,
        SessionStatus::Disconnected
    );
    let callback = Url::parse(redirect.lock().unwrap().as_ref().unwrap()).unwrap();
    assert!(
        tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, callback.port().unwrap()))
            .await
            .is_ok()
    );
    assert!(f.directory.path().join("chatgpt-host.json").exists());
}
