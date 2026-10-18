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

#[test]
fn pricing_schedule_defaults_and_custom_configuration_round_trip() {
    let credential =
        DeepSeekCredential::parse(json!({"api_key":"test-key"}).as_object().unwrap()).unwrap();
    let serialized = serde_json::to_value(&credential).unwrap();
    assert_eq!(
        serialized["pricing_schedule"],
        json!({
            "enabled":true,"timezone":"Asia/Shanghai","peak_weekdays":[1,2,3,4,5],
            "peak_windows":[{"start":"09:00","end":"12:00"},{"start":"14:00","end":"18:00"}],
            "exclude_chinese_holidays":true
        })
    );
    let schedule = json!({"enabled":false,"timezone":"America/New_York","peak_weekdays":[2,6],
        "peak_windows":[{"start":"00:00","end":"03:00"},{"start":"22:00","end":"24:00"}],
        "exclude_chinese_holidays":false});
    let credential = DeepSeekCredential::parse(
        json!({"api_key":"test-key","pricing_schedule":schedule})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&credential).unwrap()["pricing_schedule"],
        schedule
    );
}

#[test]
fn pricing_schedule_rejects_invalid_configuration_even_when_disabled() {
    for invalid in [
        json!(null),
        json!(true),
        json!({"enabled":"yes"}),
        json!({"timezone":"Mars/Olympus"}),
        json!({"peak_weekdays":[]}),
        json!({"peak_weekdays":[0]}),
        json!({"peak_weekdays":[8]}),
        json!({"peak_weekdays":[1,1]}),
        json!({"peak_windows":[]}),
        json!({"peak_windows":[{"start":"24:00","end":"24:00"}]}),
        json!({"peak_windows":[{"start":"23:00","end":"02:00"}]}),
        json!({"peak_windows":[{"start":"09:00","end":"09:00"}]}),
        json!({"peak_windows":[{"start":"9:00","end":"12:00"}]}),
        json!({"peak_windows":[{"start":"09:60","end":"12:00"}]}),
        json!({"peak_windows":[{"start":"09:00","end":"24:01"}]}),
        json!({"peak_windows":[{"start":"09:00","end":"12:00"},{"start":"11:00","end":"14:00"}]}),
        json!({"peak_windows":vec![json!({"start":"00:00","end":"01:00"});9]}),
        json!({"unknown_flag":true}),
        json!({"enabled":false,"peak_windows":[{"start":"23:00","end":"02:00"}]}),
    ] {
        assert!(
            DeepSeekCredential::parse(
                json!({"api_key":"test-key","pricing_schedule":invalid})
                    .as_object()
                    .unwrap()
            )
            .is_err(),
            "accepted {invalid}"
        );
    }
}
