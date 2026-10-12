//! 官方流式完成、错误、取消、隐私和直连请求设置用例
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use super::support::*;
use crate::*;
use bytes::Bytes;
use futures_util::stream;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn frame(event: Value) -> String {
    format!("data: {event}\r\n\r\n")
}
fn completed() -> String {
    frame(json!({"type":"response.completed","response":{"status":"completed"}}))
}
fn options() -> StreamResponseOptions {
    StreamResponseOptions {
        model: "test-model".into(),
        input: ResponseInput::Text("private clipboard text".into()),
        instructions: None,
        signal: CancellationToken::new(),
        on_delta: None,
    }
}
fn sse(wire: String) -> crate::transport::HttpResponse {
    response(
        200,
        wire.into_bytes(),
        &[
            ("content-type", "text/event-stream"),
            ("x-request-id", "req_test"),
        ],
    )
}

#[tokio::test]
async fn safe_direct_request_and_unicode_deltas() {
    let mock = Mock::new(|request| async move {
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "https://api.openai.com/v1/responses");
        assert_eq!(
            request.headers["authorization"],
            "Bearer synthetic-oauth-token"
        );
        assert!(!request.headers.contains_key("openai-organization"));
        assert!(!request.headers.contains_key("openai-project"));
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            json!({"model":"test-model","input":[{"role":"user","content":"private clipboard text"}],"instructions":"private recipe text","store":false,"stream":true})
        );
        let wire = frame(json!({"type":"response.output_text.delta","delta":"Caf"}))
            + &frame(json!({"type":"response.output_text.delta","delta":"é ☕"}))
            + &completed();
        let mut response = sse(wire.clone());
        response.body = Box::pin(stream::iter(
            wire.into_bytes()
                .into_iter()
                .map(|b| Ok(Bytes::from(vec![b]))),
        ));
        Ok(response)
    });
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let observed = deltas.clone();
    let mut opts = options();
    opts.instructions = Some("private recipe text".into());
    opts.on_delta = Some(Arc::new(move |s| {
        observed.lock().unwrap().push(s.to_owned())
    }));
    assert_eq!(
        crate::responses::stream_response(
            mock.as_ref(),
            "synthetic-oauth-token",
            &opts,
            &opts.signal
        )
        .await
        .unwrap()
        .text,
        "Café ☕"
    );
    assert_eq!(*deltas.lock().unwrap(), vec!["Caf", "é ☕"]);
    assert_eq!(mock.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn missing_content_type_and_message_field_projection() {
    let mock = Mock::new(|request| async move {
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap()["input"],
            json!([{"role":"developer","content":"Hello"}])
        );
        Ok(response(
            200,
            (frame(json!({"type":"response.output_text.delta","delta":"answer"})) + &completed())
                .into_bytes(),
            &[],
        ))
    });
    let mut opts = options();
    opts.input = serde_json::from_value(
        json!([{"role":"developer","content":"Hello","tools":"not allowed"}]),
    )
    .unwrap();
    assert_eq!(
        crate::responses::stream_response(mock.as_ref(), "synthetic", &opts, &opts.signal)
            .await
            .unwrap()
            .text,
        "answer"
    );
    assert!(
        serde_json::from_value::<ResponseInput>(json!([{"role":"system","content":"forbidden"}]))
            .is_err()
    );
}
#[tokio::test]
async fn failures_never_report_success_or_private_text() {
    let partial = frame(json!({"type":"response.output_text.delta","delta":"partial"}));
    for (wire, code, retryable) in [
        (partial.clone(), "stream_interrupted", true),
        (
            partial.clone() + "data: [DONE]\n\n",
            "stream_interrupted",
            true,
        ),
        (
            partial + &frame(json!({"type":"response.incomplete"})),
            "response_incomplete",
            true,
        ),
        (
            frame(
                json!({"type":"response.failed","response":{"error":{"code":"model_not_found","message":"private clipboard text"}}}),
            ),
            "model_not_found",
            false,
        ),
        (
            frame(
                json!({"type":"error","code":"subscription_sharing_usage_unavailable","message":"private clipboard text"}),
            ),
            "subscription_sharing_usage_unavailable",
            true,
        ),
        (
            "data: {\"private clipboard text\"\n\n".into(),
            "invalid_stream",
            true,
        ),
    ] {
        let mock = Mock::new(move |_| {
            let wire = wire.clone();
            async move { Ok(sse(wire)) }
        });
        let opts = options();
        let error =
            crate::responses::stream_response(mock.as_ref(), "synthetic", &opts, &opts.signal)
                .await
                .unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.retryable, retryable);
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("private clipboard")
        );
        if ["model_not_found", "subscription_sharing_usage_unavailable"].contains(&code) {
            assert_eq!(error.request_id.as_deref(), Some("req_test"));
        }
    }
}
#[tokio::test]
async fn cancellation_while_waiting_is_not_interruption() {
    let mock = Mock::new(|_| async {
        let mut response = sse(String::new());
        response.body = Box::pin(
            stream::once(async {
                Ok(Bytes::from(frame(
                    json!({"type":"response.output_text.delta","delta":"partial"}),
                )))
            })
            .chain(stream::pending()),
        );
        Ok(response)
    });
    use futures_util::StreamExt;
    let mut opts = options();
    let cancel = opts.signal.clone();
    opts.on_delta = Some(Arc::new(move |_| cancel.cancel()));
    assert_eq!(
        crate::responses::stream_response(mock.as_ref(), "synthetic", &opts, &opts.signal)
            .await
            .unwrap_err()
            .code,
        "cancelled"
    );
}
#[tokio::test]
async fn non_sse_success_drops_the_body() {
    struct Body {
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }
    impl futures_util::Stream for Body {
        type Item = Result<Bytes, ChatGPTError>;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }
    impl Drop for Body {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = dropped.clone();
    let mock = Mock::new(move |_| {
        let flag = flag.clone();
        async move {
            let mut r = response(200, vec![], &[("content-type", "application/json")]);
            r.body = Box::pin(Body { dropped: flag });
            Ok(r)
        }
    });
    let opts = options();
    let e = crate::responses::stream_response(mock.as_ref(), "synthetic", &opts, &opts.signal)
        .await
        .unwrap_err();
    assert_eq!(e.code, "invalid_stream");
    assert_eq!(e.status, Some(200));
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}
#[tokio::test]
async fn http_failures_sanitize_and_do_not_retry() {
    for (status, body, code, retryable) in [
        (
            401,
            json!({"error":"invalid_token","error_description":"private clipboard text"}),
            "invalid_token",
            false,
        ),
        (
            429,
            json!({"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"private clipboard text"}}),
            "subscription_sharing_usage_limit_exceeded",
            false,
        ),
        (
            422,
            json!({"detail":[{"loc":["body","input"],"type":"list_type","input":"private clipboard text"}]}),
            "api_error",
            false,
        ),
        (503, Value::Null, "api_error", true),
    ] {
        let mock = Mock::new(move |_| {
            let body = body.clone();
            async move {
                Ok(if status == 503 {
                    response(
                        503,
                        b"<html>private clipboard text</html>".to_vec(),
                        &[("x-request-id", "req_safe")],
                    )
                } else {
                    let mut r = json_response(status, body);
                    r.headers.insert("x-request-id".into(), "req_safe".into());
                    r
                })
            }
        });
        let opts = options();
        let e = crate::responses::stream_response(mock.as_ref(), "synthetic", &opts, &opts.signal)
            .await
            .unwrap_err();
        assert_eq!(e.code, code);
        assert_eq!(e.retryable, retryable);
        assert_eq!(e.status, Some(status));
        assert_eq!(e.request_id.as_deref(), Some("req_safe"));
        assert!(
            !serde_json::to_string(&e)
                .unwrap()
                .contains("private clipboard")
        );
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }
}
