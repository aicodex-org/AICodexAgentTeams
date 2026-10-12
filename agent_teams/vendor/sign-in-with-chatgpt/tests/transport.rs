//! 原生 HTTP 传输的首部与正文共同超时、错误分类及重定向拒绝用例
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use super::support::*;
use crate::{
    transport::{self, Transport},
    *,
};
use serde_json::json;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(start_paused = true)]
async fn json_deadline_includes_time_spent_waiting_for_headers() {
    for status in [200, 400, 503] {
        let mock = Mock::new(move |_| async move {
            tokio::time::sleep(Duration::from_secs(20)).await;
            let mut response = response(status, vec![], &[]);
            response.body = Box::pin(futures_util::stream::pending());
            Ok(response)
        });
        let signal = CancellationToken::new();
        let start = tokio::time::Instant::now();
        let response = transport::send(
            mock.as_ref(),
            transport::request("GET", "https://api.openai.com/v1/models"),
            &signal,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert_eq!(
            transport::json_response(response, &signal, Duration::from_secs(30))
                .await
                .unwrap_err()
                .code,
            "cancelled"
        );
        assert_eq!(start.elapsed(), Duration::from_secs(30));
    }
}
#[tokio::test]
async fn broken_json_body_retains_http_failure_classification() {
    for status in [200, 400, 503] {
        let mut response = response(status, vec![], &[("x-request-id", "req_synthetic")]);
        response.body = Box::pin(futures_util::stream::iter(vec![Err(
            crate::errors::network_error(),
        )]));
        let error =
            transport::json_response(response, &CancellationToken::new(), Duration::from_secs(1))
                .await
                .unwrap_err();
        assert_eq!(
            error.code,
            if status == 200 {
                "invalid_response"
            } else {
                "api_error"
            }
        );
        assert_eq!(error.status, Some(status));
        if status != 200 {
            assert_eq!(error.request_id.as_deref(), Some("req_synthetic"));
        }
    }
}
#[tokio::test(start_paused = true)]
async fn response_body_timeout_is_interruption_not_caller_cancellation() {
    let mock = Mock::new(|_| async {
        let mut response = response(200, vec![], &[("content-type", "text/event-stream")]);
        response.body = Box::pin(futures_util::stream::pending());
        Ok(response)
    });
    let options = StreamResponseOptions {
        model: "test".into(),
        input: ResponseInput::Text("synthetic".into()),
        instructions: None,
        signal: CancellationToken::new(),
        on_delta: None,
    };
    let error =
        crate::responses::stream_response(mock.as_ref(), "synthetic", &options, &options.signal)
            .await
            .unwrap_err();
    assert_eq!(error.code, "stream_interrupted");
    assert!(error.retryable);
    assert_eq!(
        error.message,
        "The connection was interrupted. You can keep the partial text or try again."
    );
}
#[tokio::test]
async fn default_transport_never_follows_redirects_or_forwards_body() {
    let original = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let destination = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let target = destination.local_addr().unwrap();
    let url = format!("http://{}/responses", original.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = original.accept().await.unwrap();
        let mut bytes = [0u8; 4096];
        let amount = socket.read(&mut bytes).await.unwrap();
        assert!(amount > 0);
        socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{target}/forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    });
    let mut request = transport::request("POST", &url);
    request.body = serde_json::to_vec(&json!({"input":"synthetic private text"})).unwrap();
    let result = transport::ReqwestTransport::new()
        .unwrap()
        .send(request)
        .await;
    assert_eq!(result.err().unwrap().code, "network_error");
    server.await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), destination.accept())
            .await
            .is_err()
    );
}
