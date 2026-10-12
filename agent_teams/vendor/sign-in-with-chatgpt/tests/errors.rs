//! 官方 JSON 取消分类、模型目录与安全错误诊断用例
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use super::support::*;
use crate::{
    errors::api_error,
    transport::{self, HttpResponse},
    *,
};
use bytes::Bytes;
use futures_util::stream;
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};

#[tokio::test]
async fn interrupted_json_bodies_remain_cancellation() {
    for _name in ["AbortError", "TimeoutError"] {
        for status in [200, 400, 503] {
            let response = HttpResponse {
                status,
                headers: BTreeMap::new(),
                body: Box::pin(stream::iter(vec![
                    Ok(Bytes::from_static(b"{\"models\":[")),
                    Err(crate::errors::cancelled()),
                ])),
            };
            let e = transport::json_response(
                response,
                &CancellationToken::new(),
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
            assert_eq!(e.code, "cancelled");
            assert!(!e.retryable);
        }
    }
}
#[tokio::test]
async fn model_body_cancellation_after_headers() {
    let signal = CancellationToken::new();
    let cancel = signal.clone();
    let mock = Mock::new(move |_| {
        let cancel = cancel.clone();
        async move {
            cancel.cancel();
            Ok(json_response(200, json!({"models":[]})))
        }
    });
    assert_eq!(
        crate::responses::list_models(mock.as_ref(), "synthetic-token", &signal)
            .await
            .unwrap_err()
            .code,
        "cancelled"
    );
}
#[tokio::test]
async fn malformed_and_valid_json_contracts() {
    let malformed = response(200, b"{not json".to_vec(), &[]);
    let e = transport::json_response(malformed, &CancellationToken::new(), Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(e.code, "invalid_response");
    assert_eq!(e.status, Some(200));
    assert!(e.retryable);
    let malformed = response(
        503,
        b"<html>Unavailable</html>".to_vec(),
        &[("x-request-id", "synthetic-request-id")],
    );
    let e = transport::json_response(malformed, &CancellationToken::new(), Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(e.code, "api_error");
    assert_eq!(e.status, Some(503));
    assert_eq!(e.request_id.as_deref(), Some("synthetic-request-id"));
    assert!(e.retryable);
    assert_eq!(
        transport::json_response(
            json_response(200, json!({"models":[]})),
            &CancellationToken::new(),
            Duration::from_secs(1)
        )
        .await
        .unwrap(),
        json!({"models":[]})
    );
}
#[test]
fn errors_keep_exact_code_but_never_server_input() {
    for code in [
        "subscription_sharing_usage_limit_exceeded",
        "subscription_sharing_usage_unavailable",
        "subscription_sharing_v2_user_not_eligible",
        "invalid_grant",
        "invalid_token",
        "invalid_api_key",
        "access_denied",
        "model_not_found",
    ] {
        let e = api_error(
            Some(&json!({"error":{"code":code,"message":"private clipboard text"}})),
            Some(401),
            Some("req_safe"),
        );
        assert_eq!(e.code, code);
        assert!(
            !serde_json::to_string(&e)
                .unwrap()
                .contains("private clipboard")
        );
        assert_eq!(e.request_id.as_deref(), Some("req_safe"));
    }
    let e = api_error(
        Some(
            &json!({"detail":[{"loc":["body","input"],"type":"list_type","input":"private clipboard text"}]}),
        ),
        Some(422),
        Some("unsafe request id\n"),
    );
    assert_eq!(
        e.message,
        "ChatGPT requires input as a list of messages. Update the app's request format."
    );
    assert_eq!(e.request_id, None);
    assert!(e.response_shape.unwrap().contains("input:redacted"));
    let e = api_error(Some(&json!({"error":"invalid_token"})), Some(401), None);
    assert!(!crate::errors::requires_reauthentication(&e));
}
#[tokio::test]
async fn model_catalog_is_authoritative_and_validated() {
    for (catalog, expected) in [
        (json!({"data":[{"id":"fallback"}]}), false),
        (
            json!({"models":[{"visibility":"list","slug":"","display_name":"Bad"}]}),
            false,
        ),
        (
            json!({"models":[{"visibility":"hidden"},{"visibility":"list","slug":"real","display_name":"Real"}]}),
            true,
        ),
    ] {
        let mock = Mock::new(move |request| {
            let catalog = catalog.clone();
            async move {
                assert_eq!(request.url, "https://api.openai.com/v1/models");
                Ok(json_response(200, catalog))
            }
        });
        let result =
            crate::responses::list_models(mock.as_ref(), "synthetic", &CancellationToken::new())
                .await;
        if expected {
            assert_eq!(result.unwrap()[0].slug, "real");
        } else {
            assert_eq!(result.unwrap_err().code, "invalid_model_catalog");
        }
    }
}
