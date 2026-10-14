//! DeepSeek 凭据与原生端点配置合同测试

use provider_deepseek::DeepSeekCredential;
use serde_json::json;

#[test]
fn one_key_derives_three_native_endpoints() {
    let credential =
        DeepSeekCredential::parse(json!({"api_key":"test-key"}).as_object().unwrap()).unwrap();
    assert_eq!(
        credential.endpoint("openai").unwrap(),
        "https://api.deepseek.com/responses"
    );
    assert_eq!(
        credential.endpoint("openai-chat").unwrap(),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        credential.endpoint("anthropic").unwrap(),
        "https://api.deepseek.com/anthropic/v1/messages"
    );
}
#[test]
fn explicit_protocols_are_enforced() {
    let credential = DeepSeekCredential::parse(
        json!({"api_key":"test-key","protocols":["openai"]})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    assert!(credential.endpoint("anthropic").is_err());
}
#[test]
fn credentials_reject_ambiguous_or_unsafe_url_fields() {
    for base in [
        "https://secret@example.com",
        "file:///tmp/key",
        "https://example.com?key=secret",
        "https://example.com/#fragment",
    ] {
        assert!(
            DeepSeekCredential::parse(
                json!({"api_key":"test-key","base_url":base})
                    .as_object()
                    .unwrap()
            )
            .is_err()
        );
    }
    assert!(
        DeepSeekCredential::parse(json!({"api_key":"\r\ninjected"}).as_object().unwrap()).is_err()
    );
}

#[test]
fn only_omitted_or_http_transport_is_accepted() {
    for value in [
        json!({"api_key":"test-key"}),
        json!({"api_key":"test-key","transport":"http"}),
    ] {
        assert!(DeepSeekCredential::parse(value.as_object().unwrap()).is_ok());
    }
    for transport in [
        json!("prefer_websocket"),
        json!("websocket"),
        json!("http_sse"),
        json!(""),
        json!(null),
        json!(true),
    ] {
        let value = json!({"api_key":"test-key","transport":transport});
        let error = DeepSeekCredential::parse(value.as_object().unwrap()).unwrap_err();
        assert_eq!(
            error.kind(),
            gateway_admin::ports::provider::ProviderAdminErrorKind::Invalid
        );
    }
}
