//! Jev 原生推理、账号管理、限流与资源生命周期的隔离上游验证
#![allow(dead_code)]
mod pricing;
mod schema;
mod support;

use bytes::Bytes;
use futures::StreamExt;
use gateway_admin::{model::provider_credentials::*, ports::provider::ProviderAdmin};
use gateway_core::{
    account::OpaqueProviderData,
    engine::provider::Provider,
    error::ProviderErrorKind,
    event::GatewayEvent,
    lifecycle::CancellationToken,
    operation::{Operation, RawJsonPayload, StructuredInferenceRequest},
    upstream::UpstreamSendState,
};
use serde_json::{Value, json};
use support::{context, fixture, request};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

fn operation_raw(raw: impl Into<Bytes>) -> Operation {
    Operation::StructuredInference(StructuredInferenceRequest::from_raw_json(
        RawJsonPayload::new("typesafe-systemone", raw.into()).unwrap(),
    ))
}
fn operation() -> Operation {
    operation_raw(Bytes::from_static(br#"{ "model":"jev-latest", "state":{"image":"ordinary business key"}, "questions":{"q":{"type":"noul","instructions":"Is safe?"}},"extension":{"kept":true} }"#))
}
fn response() -> Value {
    json!({"model":"jev-1.13.0","answers":{"q":{"type":"noul","noul":0.8}},"usage":{"input_tokens":100,"output_tokens":0}})
}
fn document(value: Value) -> ProviderDocument {
    ProviderDocument::new(OpaqueProviderData::new(value.as_object().unwrap().clone()))
}

#[tokio::test]
async fn native_json_preserves_wire_and_full_usage_and_price() {
    let server = MockServer::start().await;
    let raw = format!(" {}\n", response());
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer synthetic-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-typesafe-request-id", "jev-test-request")
                .set_body_raw(raw.clone(), "application/json"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let mut stream = provider
        .execute(request(operation()), context(CancellationToken::new()))
        .await
        .unwrap();
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "execute must not send before polling"
    );
    let event = stream.next().await.unwrap().unwrap();
    assert_eq!(
        event
            .wire_event()
            .unwrap()
            .raw_json_body()
            .unwrap()
            .as_ref(),
        raw.as_bytes()
    );
    assert_eq!(
        event
            .response_observation()
            .unwrap()
            .upstream_response_model(),
        Some("jev-1.13.0")
    );
    let mut usage = None;
    let mut cost = None;
    for fact in event.canonical_facts() {
        match fact {
            GatewayEvent::Usage(value) => usage = Some(value),
            GatewayEvent::CalculatedCost(value) => cost = Some(value.total().amount().canonical()),
            GatewayEvent::Completed(meta) => {
                assert_eq!(meta.response_id(), "jev-test-request");
            }
            _ => {}
        }
    }
    assert_eq!(usage.unwrap().input_tokens, Some(100));
    assert_eq!(cost.as_deref(), Some("0.0000042"));
    assert!(stream.next().await.is_none());
    let requests = server.received_requests().await.unwrap();
    let Operation::StructuredInference(inference) = operation() else {
        unreachable!()
    };
    assert_eq!(requests[0].body, inference.payload().body().as_ref());
}

#[tokio::test]
async fn invalid_requests_are_rejected_without_network() {
    let server = MockServer::start().await;
    let (provider, _, _) = fixture(&server.uri()).await;
    for raw in [
        r#"{"model":"jev-latest","model":"jev-latest","state":"x","questions":{"q":{"type":"noul","instructions":"x"}}}"#,
        r#"{"model":"jev-latest","state":{"a":1,"a":2},"questions":{"q":{"type":"noul","instructions":"x"}}}"#,
        r#"{"model":"jev-latest","stream":true,"state":"x","questions":{"q":{"type":"noul","instructions":"x"}}}"#,
        r#"{"model":"jev-latest","assets":{"orphan":{}},"state":"x","questions":{"q":{"type":"noul","instructions":"x"}}}"#,
        r#"{"model":"jev-latest","state":"x","questions":{"q":{"type":"box","instructions":"x"}}}"#,
        r#"{"model":"jev-latest","state":"x","questions":{"q":{"type":"score","instructions":"x","criteria":["one"]}}}"#,
        r#"{"model":"jev-latest","state":"x","questions":{"q":{"type":"noul"}}}"#,
        r#"{"model":"different-route","state":"x","questions":{"q":{"type":"noul","instructions":"x"}}}"#,
    ] {
        let error = match provider
            .clone()
            .execute(
                request(operation_raw(Bytes::copy_from_slice(raw.as_bytes()))),
                context(CancellationToken::new()),
            )
            .await
        {
            Ok(_) => panic!("accepted {raw}"),
            Err(error) => error,
        };
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn organization_limit_does_not_rotate_or_capture_secrets() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "30")
                .set_body_raw(
                    r#"{"error":"synthetic-key \u0073ynthetic-key"}"#,
                    "application/json",
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let mut stream = provider
        .execute(request(operation()), context(CancellationToken::new()))
        .await
        .unwrap();
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
    assert!(error.retry_is_prohibited());
    assert_eq!(
        error.retry_after(),
        Some(std::time::Duration::from_secs(30))
    );
    assert!(!format!("{error:?}").contains("synthetic-key"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn missing_usage_stays_unknown_and_malformed_answer_is_failure() {
    for (value, valid) in [
        (
            json!({"model":"jev-1.13.0","answers":{"q":{"type":"noul","noul":0.5}}}),
            true,
        ),
        (
            json!({"model":"jev-1.13.0","answers":{"q":{"type":"noul","noul":1.2}}}),
            false,
        ),
        (
            json!({"model":"jev-1.13.0","answers":{},"usage":{"input_tokens":1,"output_tokens":0}}),
            false,
        ),
        (
            json!({"model":"jev-1.13.0","answers":{"q":{"type":"noul","noul":0.5}},"usage":{"input_tokens":"10"}}),
            false,
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(path("/v1/systemone"))
            .respond_with(ResponseTemplate::new(200).set_body_json(value))
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let mut stream = provider
            .execute(request(operation()), context(CancellationToken::new()))
            .await
            .unwrap();
        let event = stream.next().await.unwrap();
        if valid {
            let event = event.unwrap();
            assert!(event.canonical_facts().iter().any(|f| matches!(f, GatewayEvent::Usage(usage) if usage.input_tokens.is_none() && usage.output_tokens.is_none())));
            assert!(
                !event
                    .canonical_facts()
                    .iter()
                    .any(|f| matches!(f, GatewayEvent::CalculatedCost(_)))
            );
        } else {
            assert_eq!(event.unwrap_err().kind(), ProviderErrorKind::Protocol);
        }
    }
}

#[tokio::test]
async fn cold_drop_and_pre_send_cancel_never_reach_upstream() {
    let server = MockServer::start().await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let stream = provider
        .clone()
        .execute(request(operation()), context(CancellationToken::new()))
        .await
        .unwrap();
    drop(stream);
    let cancel = CancellationToken::new();
    let mut stream = provider
        .clone()
        .execute(request(operation()), context(cancel.clone()))
        .await
        .unwrap();
    cancel.cancel();
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    drop(stream);
    // 第三个租约仍可获取，证明两个未发送流已释放账号容量
    let stream = provider
        .execute(request(operation()), context(CancellationToken::new()))
        .await
        .unwrap();
    drop(stream);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn cancel_after_send_is_ambiguous_and_releases_capacity() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(10))
                .set_body_json(response()),
        )
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let cancel = CancellationToken::new();
    let mut stream = provider
        .clone()
        .execute(request(operation()), context(cancel.clone()))
        .await
        .unwrap();
    let handle = tokio::spawn(async move { stream.next().await.unwrap().unwrap_err() });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    let error = handle.await.unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
    let next = provider
        .execute(request(operation()), context(CancellationToken::new()))
        .await
        .unwrap();
    drop(next);
}

#[tokio::test]
async fn imports_and_rotation_preserve_extensions_and_hide_key_in_configuration() {
    let (provider, store, id) = fixture("https://api.typesafe.ai").await;
    let imported = provider
        .prepare_import(PrepareCredentialImport {
            default_outbound_proxy: None,
            document: document(json!({"api_key":"new-synthetic","future":{"enabled":true}})),
        })
        .await
        .unwrap();
    assert_eq!(imported.credentials[0].name, "Jev");
    assert_eq!(
        imported.credentials[0]
            .provider_material
            .expose_to_provider()
            .expose_to_provider()["future"],
        json!({"enabled":true})
    );
    let rotated = provider
        .prepare_rotation(PrepareCredentialRotation {
            account: support::account_record(&store.account(&id).unwrap()),
            provider_material: document(json!({"api_key":"rotated-synthetic","future":42})),
        })
        .await
        .unwrap();
    assert_eq!(
        rotated
            .facts()
            .provider_material
            .expose_to_provider()
            .expose_to_provider()["future"],
        42
    );
    assert!(
        provider
            .account_configuration(&id)
            .await
            .unwrap()
            .unwrap()
            .expose_to_provider()
            .expose_to_provider()
            .get("api_key")
            .is_none()
    );
}

#[tokio::test]
async fn models_native_catalog_caches_without_rejecting_unknown_fixed_ids() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/models")).and(header("authorization","Bearer synthetic-key")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":[{"name":"jev-latest","description":"Latest","release_date":"2025-01-01"},{"name":"jev-preview","description":"Preview","release_date":"2025-01-02"}]}))).expect(1).mount(&server).await;
    let (provider, _, id) = fixture(&server.uri()).await;
    let models = provider.models(&id, false).await.unwrap();
    assert_eq!(models.models.len(), 2);
    assert_eq!(provider.models(&id, false).await.unwrap().models.len(), 2);
    assert!(!provider.model_catalog_is_exhaustive());
    let probe = provider
        .connection_test_operation(
            &gateway_core::routing::UpstreamModelId::new("jev-1.14.0").unwrap(),
            "test",
        )
        .await
        .unwrap();
    assert_eq!(
        probe.kind(),
        gateway_core::operation::OperationKind::StructuredInference
    );
}

#[tokio::test]
async fn repeated_successful_requests_release_leases_on_drop() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response()))
        .expect(8)
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    for _ in 0..8 {
        let mut stream = provider
            .clone()
            .execute(request(operation()), context(CancellationToken::new()))
            .await
            .unwrap();
        stream.next().await.unwrap().unwrap();
        // 模拟 HTTP 出口读到 Completed 后直接释放，不要求再次 poll 到 EOF
        drop(stream);
    }
}

#[tokio::test]
async fn upstream_auth_failure_invalidates_only_the_selected_key() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(401).set_body_string("synthetic-key"))
        .mount(&server)
        .await;
    let (provider, store, id) = fixture(&server.uri()).await;
    let mut stream = provider
        .execute(request(operation()), context(CancellationToken::new()))
        .await
        .unwrap();
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Unauthorized);
    assert_eq!(
        store.account(&id).unwrap().credential_state(),
        gateway_core::account::CredentialState::Invalid
    );
}

#[tokio::test]
async fn parameter_rejection_does_not_change_account_state() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(422))
        .mount(&server)
        .await;
    let (provider, store, id) = fixture(&server.uri()).await;
    let mut stream = provider
        .execute(request(operation()), context(CancellationToken::new()))
        .await
        .unwrap();
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::InvalidRequest);
    assert!(error.retry_is_prohibited());
    assert_eq!(
        store.account(&id).unwrap().credential_state(),
        gateway_core::account::CredentialState::Ready
    );
}

#[tokio::test]
async fn encoded_secret_echo_is_not_forwarded_as_a_success() {
    let server = MockServer::start().await;
    let mut body = response();
    body["extension"] = json!(r#"{"echo":"\u0073ynthetic-key"}"#);
    Mock::given(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let mut stream = provider
        .execute(request(operation()), context(CancellationToken::new()))
        .await
        .unwrap();
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Protocol);
    assert!(error.retry_is_prohibited());
    assert!(!format!("{error:?}").contains("synthetic-key"));
}

#[tokio::test]
async fn caller_deadline_after_send_remains_ambiguous() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_json(response()),
        )
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let context = support::context_with_budget(
        CancellationToken::new(),
        Default::default(),
        gateway_core::engine::execution::ClientTransport::InternalProbe,
        std::time::Duration::from_secs(5),
    );
    let mut stream = provider
        .execute(request(operation()), context)
        .await
        .unwrap();
    let handle = tokio::spawn(async move { stream.next().await.unwrap().unwrap_err() });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("upstream must have received the request before its deadline");
    let error = handle.await.unwrap();
    assert_eq!(error.kind(), ProviderErrorKind::Timeout);
    assert_eq!(error.send_state(), UpstreamSendState::Ambiguous);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
