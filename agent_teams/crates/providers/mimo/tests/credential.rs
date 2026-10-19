//! MiMo 模式、区域、密钥保护与三协议端点合同测试

use provider_mimo::MiMoCredential;
use serde_json::json;

#[test]
fn official_modes_regions_and_protocols_resolve_without_duplicate_version() {
    for host in [
        "api.xiaomimimo.com",
        "token-plan-cn.xiaomimimo.com",
        "token-plan-sgp.xiaomimimo.com",
        "token-plan-ams.xiaomimimo.com",
    ] {
        for root in ["", "/v1", "/anthropic", "/anthropic/v1"] {
            let credential = MiMoCredential::parse(
                json!({"api_key":"synthetic-key", "base_url":format!("https://{host}{root}")})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                credential.endpoint("openai").unwrap(),
                format!("https://{host}/v1/responses")
            );
            assert_eq!(
                credential.endpoint("openai-chat").unwrap(),
                format!("https://{host}/v1/chat/completions")
            );
            assert_eq!(
                credential.endpoint("anthropic").unwrap(),
                format!("https://{host}/anthropic/v1/messages")
            );
        }
    }
    for (key, mode, host) in [
        ("sk-test", "api_key", "api.xiaomimimo.com"),
        ("tp-test", "token_plan", "token-plan-cn.xiaomimimo.com"),
        ("ttp-test", "token_plan", "token-plan-cn.xiaomimimo.com"),
    ] {
        let credential =
            MiMoCredential::parse(json!({"api_key":key}).as_object().unwrap()).unwrap();
        assert_eq!(
            credential.endpoint("openai").unwrap(),
            format!("https://{host}/v1/responses")
        );
        assert_eq!(
            serde_json::to_value(&credential).unwrap()["access_mode"],
            mode
        );
        assert!(!format!("{credential:?}").contains(key));
    }
}

#[test]
fn custom_prefix_is_preserved_without_guessing_official_anthropic_path() {
    let credential = MiMoCredential::parse(json!({"api_key":"test-key", "access_mode":"token_plan", "base_url":"https://relay.example/prefix/v1/"}).as_object().unwrap()).unwrap();
    assert_eq!(
        credential.endpoint("openai").unwrap(),
        "https://relay.example/prefix/v1/responses"
    );
    assert_eq!(
        credential.endpoint("openai-chat").unwrap(),
        "https://relay.example/prefix/v1/chat/completions"
    );
    assert_eq!(
        credential.endpoint("anthropic").unwrap(),
        "https://relay.example/prefix/v1/messages"
    );
    let credential = MiMoCredential::parse(
        json!({"api_key":"test-key", "base_url":"https://api.xiaomimimo.com.evil/v1"})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        credential.endpoint("anthropic").unwrap(),
        "https://api.xiaomimimo.com.evil/v1/messages"
    );
}

#[test]
fn invalid_modes_protocols_urls_and_headers_are_rejected() {
    for patch in [
        json!({"access_mode":"unknown"}),
        json!({"access_mode":null}),
        json!({"protocols":[]}),
        json!({"protocols":["invalid"]}),
        json!({"protocols":null}),
        json!({"api_key":"key\r\nheader"}),
        json!({"api_key":" key "}),
        json!({"api_key":""}),
        json!({"transport":"prefer_websocket"}),
        json!({"base_url":"https://user:pass@example.com/v1"}),
        json!({"base_url":"file:///tmp/secret"}),
        json!({"base_url":"https://example.com/v1?secret=key"}),
        json!({"base_url":"https://example.com/#secret"}),
    ] {
        let mut value = json!({"api_key":"test-key"});
        value
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        assert!(
            MiMoCredential::parse(value.as_object().unwrap()).is_err(),
            "accepted {patch}"
        );
    }
    let credential = MiMoCredential::parse(
        json!({"api_key":"test-key", "protocols":["openai-chat"]})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    assert!(credential.endpoint("openai-chat").is_ok());
    assert!(credential.endpoint("openai").is_err());
    assert!(credential.endpoint("anthropic").is_err());
}
