//! 三协议原生线格式、计量、终态、cold 发送与取消边界验证
use crate::support::{context, context_with_transport, execution_fixture as fixture, request};
use futures::StreamExt;
use gateway_core::{
    engine::provider::Provider,
    event::GatewayEvent,
    lifecycle::CancellationToken,
    operation::{GenerateRequest, Operation, ProtocolPayload},
};
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

fn operation(protocol: &str, stream: bool) -> Operation {
    let body = match protocol {
        "openai" => json!({"model":"deepseek-flash","input":"hello","stream":stream}),
        _ => {
            json!({"model":"deepseek-flash","messages":[{"role":"user","content":"hello"}],"stream":stream})
        }
    };
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(protocol, body.as_object().unwrap().clone()).unwrap(),
    ))
}

#[tokio::test]
async fn native_streams_preserve_bytes_and_complete_usage() {
    for (protocol, url, raw) in [
        (
            "openai",
            "/responses",
            ": keepalive\r\n\r\ndata:\n\nevent: response.created\r\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\",\"model\":\"deepseek-flash\"}}\r\n\r\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2},\"output\":[]}}\n\n",
        ),
        (
            "openai-chat",
            "/v1/chat/completions",
            "data: {\"id\":\"r1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"r1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n",
        ),
        (
            "anthropic",
            "/anthropic/v1/messages",
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"r1\",\"usage\":{\"input_tokens\":8,\"cache_read_input_tokens\":2,\"output_tokens\":0}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(url))
            .and(header("authorization", "Bearer synthetic-key"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(raw, "text/event-stream"))
            .expect(1)
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let mut stream = provider
            .execute(
                request(operation(protocol, true)),
                context_with_transport(
                    CancellationToken::new(),
                    gateway_core::engine::execution::ClientTransport::HttpSse,
                ),
            )
            .await
            .unwrap();
        assert_eq!(stream.metadata().transport().as_str(), "http_sse");
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "execute must remain cold"
        );
        let mut bytes = Vec::new();
        let mut usage = gateway_core::metering::Usage::new();
        let mut completed = 0;
        while let Some(event) = stream.next().await {
            let event = event.unwrap_or_else(|e| panic!("{protocol}: {e:?}"));
            if let Some(observation) = event.response_observation() {
                assert_eq!(observation.transport().as_str(), "http_sse");
            }
            if let Some(wire) = event.wire_event() {
                bytes.extend_from_slice(wire.raw_sse_frame().unwrap());
            }
            for fact in event.canonical_facts() {
                match fact {
                    GatewayEvent::Usage(value) => usage.merge(value),
                    GatewayEvent::Completed(_) => completed += 1,
                    _ => {}
                }
            }
        }
        assert_eq!(bytes, raw.as_bytes(), "{protocol}");
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(2));
        assert_eq!(completed, 1);
    }
}

#[tokio::test]
async fn native_nonstream_keeps_raw_json() {
    for (protocol, url, raw) in [
        (
            "openai-chat",
            "/v1/chat/completions",
            r#"{ "id":"r1", "choices":[{"message":{"content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1},"unknown":{"kept":true}}"#,
        ),
        (
            "anthropic",
            "/anthropic/v1/messages",
            r#"{ "id":"r1", "type":"message", "content":[{"type":"text","text":"hi"}], "stop_reason":"end_turn","usage":{"input_tokens":3,"output_tokens":1}}"#,
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(path(url))
            .respond_with(ResponseTemplate::new(200).set_body_raw(raw, "application/json"))
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let mut stream = provider
            .execute(
                request(operation(protocol, false)),
                context_with_transport(
                    CancellationToken::new(),
                    gateway_core::engine::execution::ClientTransport::HttpJson,
                ),
            )
            .await
            .unwrap();
        assert_eq!(stream.metadata().transport().as_str(), "http_json");
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(
            event.response_observation().unwrap().transport().as_str(),
            "http_json"
        );
        assert_eq!(
            event
                .wire_event()
                .unwrap()
                .raw_json_body()
                .unwrap()
                .as_ref(),
            raw.as_bytes()
        );
        assert!(stream.next().await.is_none());
        let requests = server.received_requests().await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["stream"], false);
    }
}

#[tokio::test]
async fn dropped_cold_stream_and_cancelled_request_do_not_send() {
    let server = MockServer::start().await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let cancellation = CancellationToken::new();
    let stream = provider
        .clone()
        .execute(
            request(operation("openai-chat", true)),
            context(cancellation.clone()),
        )
        .await
        .unwrap();
    drop(stream);
    assert!(server.received_requests().await.unwrap().is_empty());
    cancellation.cancel();
    let error = provider
        .execute(request(operation("anthropic", true)), context(cancellation))
        .await
        .err()
        .unwrap();
    assert_eq!(
        error.kind(),
        gateway_core::error::ProviderErrorKind::Cancelled
    );
}

#[tokio::test]
async fn truncated_stream_is_not_success() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "data: {\"id\":\"r1\",\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "text/event-stream",
        ))
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let mut stream = provider
        .execute(
            request(operation("openai-chat", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    assert!(stream.next().await.unwrap().is_err());
}

#[tokio::test]
async fn three_protocols_share_capacity_and_dropping_stream_releases_it() {
    let server = MockServer::start().await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let first = provider
        .clone()
        .execute(
            request(operation("openai", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    let second = provider
        .clone()
        .execute(
            request(operation("openai-chat", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    let blocked = provider
        .clone()
        .execute(
            request(operation("anthropic", true)),
            context(CancellationToken::new()),
        )
        .await
        .err()
        .unwrap();
    assert!(matches!(
        blocked.kind(),
        gateway_core::error::ProviderErrorKind::NoEligibleAccount
            | gateway_core::error::ProviderErrorKind::AccountCapacityUnavailable
    ));
    drop(first);
    let third = provider
        .execute(
            request(operation("anthropic", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    drop((second, third));
    assert!(server.received_requests().await.unwrap().is_empty());
}
#[tokio::test]
async fn http_error_preserves_status_body_and_retry_after_without_key_echo() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions")).respond_with(ResponseTemplate::new(429).insert_header("retry-after","7").set_body_json(json!({"error":{"message":"synthetic-key rate limited","type":"rate_limit_error"}}))).mount(&server).await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let mut stream = provider
        .execute(
            request(operation("openai-chat", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(
        error.kind(),
        gateway_core::error::ProviderErrorKind::RateLimited
    );
    assert_eq!(error.retry_after(), Some(std::time::Duration::from_secs(7)));
    let response = error.client_visible_upstream_response().unwrap();
    assert_eq!(response.status(), 429);
    let body = String::from_utf8_lossy(response.body());
    assert!(body.contains("rate limited"));
    assert!(!body.contains("synthetic-key"));
}
#[tokio::test]
async fn sse_business_failure_preserves_original_terminal_frame() {
    let server = MockServer::start().await;
    let raw = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"tool input invalid\"}}\n\n";
    Mock::given(path("/anthropic/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(raw, "text/event-stream"))
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let mut stream = provider
        .execute(
            request(operation("anthropic", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(
        error.kind(),
        gateway_core::error::ProviderErrorKind::InvalidRequest
    );
    let events = error.take_atomic_client_events();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]
            .wire_event()
            .unwrap()
            .raw_sse_frame()
            .unwrap()
            .as_ref(),
        raw.as_bytes()
    );
}

#[tokio::test]
async fn configured_cost_uses_sent_model_cache_tokens_and_multiplier() {
    use crate::support::context_with_pricing;
    use gateway_core::metering::{ModelPriceOverride, TokenPriceOverride};
    use std::collections::BTreeMap;
    let price = ModelPriceOverride {
        multiplier_bps: 20000,
        bands: BTreeMap::from([(
            "standard".to_owned(),
            TokenPriceOverride {
                input: "2".to_owned().try_into().unwrap(),
                output: "4".to_owned().try_into().unwrap(),
                cache_read: "1".to_owned().try_into().unwrap(),
                cache_write: "2".to_owned().try_into().unwrap(),
            },
        )]),
    };
    for (has_price, has_usage, tier, expected) in [
        (true, true, None, Some("0.00044")),
        (false, true, None, None),
        (true, false, None, None),
        (true, true, Some("auto"), None),
        (true, true, Some("fast"), None),
    ] {
        let server = MockServer::start().await;
        let mut body = json!({"id":"r1","model":"different-echo-name","choices":[{"message":{"content":"hi"},"finish_reason":"stop"}]});
        if has_usage {
            body["usage"] =
                json!({"prompt_tokens":100,"completion_tokens":10,"prompt_cache_hit_tokens":20});
        }
        Mock::given(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let pricing = if has_price {
            BTreeMap::from([(
                "deepseek".to_owned(),
                BTreeMap::from([("deepseek-flash".to_owned(), price.clone())]),
            )])
        } else {
            BTreeMap::new()
        };
        let mut payload = json!({"model":"deepseek-flash","messages":[{"role":"user","content":"hi"}],"stream":false});
        if let Some(tier) = tier {
            payload["service_tier"] = json!(tier);
        }
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai-chat", payload.as_object().unwrap().clone())
                .unwrap(),
        ));
        let mut stream = provider
            .execute(
                request(operation),
                context_with_pricing(CancellationToken::new(), pricing),
            )
            .await
            .unwrap();
        let event = stream.next().await.unwrap().unwrap();
        let cost = event.canonical_facts().iter().find_map(|event| {
            if let GatewayEvent::CalculatedCost(cost) = event {
                Some(cost.total().amount().canonical())
            } else {
                None
            }
        });
        assert_eq!(cost.as_deref(), expected);
    }
}

#[tokio::test]
async fn cancellation_after_send_is_ambiguous_and_drops_account_lease() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(10))
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let cancellation = CancellationToken::new();
    let mut stream = provider
        .clone()
        .execute(
            request(operation("openai-chat", false)),
            context(cancellation.clone()),
        )
        .await
        .unwrap();
    let task = tokio::spawn(async move { stream.next().await.unwrap().unwrap_err() });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    cancellation.cancel();
    let error = task.await.unwrap();
    assert_eq!(
        error.kind(),
        gateway_core::error::ProviderErrorKind::Cancelled
    );
    assert_eq!(
        error.send_state(),
        gateway_core::upstream::UpstreamSendState::Ambiguous
    );
    let first = provider
        .clone()
        .execute(
            request(operation("openai", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    let second = provider
        .execute(
            request(operation("anthropic", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    drop((first, second));
}

#[tokio::test(start_paused = true)]
async fn catalog_generation_expires_without_configuration_changes() {
    let provider = provider_deepseek::DeepSeekProvider::new(crate::support::ports(
        std::sync::Arc::new(crate::support::MemoryProviderAccountStore::default()),
    ))
    .unwrap();
    let initial = provider.catalog_generation();
    tokio::time::advance(std::time::Duration::from_secs(299)).await;
    assert_eq!(provider.catalog_generation(), initial);
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert_ne!(provider.catalog_generation(), initial);
    let refreshed = provider.catalog_generation();
    tokio::time::advance(std::time::Duration::from_secs(300)).await;
    assert_ne!(provider.catalog_generation(), refreshed);
}

#[tokio::test]
async fn no_accounts_is_known_empty_but_unreadable_accounts_are_unknown() {
    let empty = provider_deepseek::DeepSeekProvider::new(crate::support::ports(
        std::sync::Arc::new(crate::support::MemoryProviderAccountStore::default()),
    ))
    .unwrap();
    assert!(empty.query_model_capabilities().await.unwrap().is_empty());
    let (unavailable, _, _) = crate::support::fixture("http://127.0.0.1:1").await;
    assert_eq!(
        unavailable
            .query_model_capabilities()
            .await
            .unwrap_err()
            .kind(),
        gateway_core::error::ProviderErrorKind::ProviderInfrastructureUnavailable
    );
}

#[tokio::test]
async fn websocket_is_rejected_before_account_selection_even_with_http_fallback() {
    use gateway_core::{
        engine::execution::ClientTransport,
        error::ProviderErrorKind,
        upstream::{AttemptTransport, UpstreamSendState},
    };
    let server = MockServer::start().await;
    let (provider, store, _) = fixture(&server.uri()).await;
    for protocol in provider_deepseek::PROTOCOLS {
        for transport in [AttemptTransport::Default, AttemptTransport::Fallback] {
            let error = provider
                .clone()
                .execute(
                    request(operation(protocol, true)),
                    context_with_transport(CancellationToken::new(), ClientTransport::WebSocket)
                        .with_transport(transport),
                )
                .await
                .err()
                .expect("WebSocket unsupported");
            assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        }
    }
    assert_eq!(
        store.provider_listing_reads(),
        0,
        "WebSocket must not enter account selection or acquire a lease"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    let first = provider
        .clone()
        .execute(
            request(operation("openai", true)),
            context_with_transport(CancellationToken::new(), ClientTransport::HttpSse),
        )
        .await
        .unwrap();
    let second = provider
        .execute(
            request(operation("anthropic", false)),
            context_with_transport(CancellationToken::new(), ClientTransport::HttpJson),
        )
        .await
        .unwrap();
    drop((first, second));
}

#[tokio::test]
async fn all_protocol_http_errors_preserve_classification_and_native_details() {
    use gateway_core::{error::ProviderErrorKind, upstream::UpstreamSendState};
    for (protocol, url) in [
        ("openai", "/responses"),
        ("openai-chat", "/v1/chat/completions"),
        ("anthropic", "/anthropic/v1/messages"),
    ] {
        for (status, kind) in [
            (401, ProviderErrorKind::Unauthorized),
            (402, ProviderErrorKind::QuotaExhausted),
            (429, ProviderErrorKind::RateLimited),
            (500, ProviderErrorKind::Unavailable),
        ] {
            let server = MockServer::start().await;
            Mock::given(path(url)).respond_with(ResponseTemplate::new(status).insert_header("retry-after", "9").set_body_json(json!({"error":{"message":"synthetic-key rejected","type":"upstream_error","code":format!("synthetic_{status}")}}))).expect(1).mount(&server).await;
            let (provider, _, _) = fixture(&server.uri()).await;
            let mut stream = provider
                .execute(
                    request(operation(protocol, true)),
                    context(CancellationToken::new()),
                )
                .await
                .unwrap();
            let error = stream.next().await.unwrap().unwrap_err();
            assert_eq!(error.kind(), kind, "{protocol}: {status}");
            assert_eq!(
                error.upstream_code().unwrap().as_str(),
                format!("synthetic_{status}")
            );
            assert_eq!(error.send_state(), UpstreamSendState::Sent);
            assert_eq!(error.retry_after(), Some(std::time::Duration::from_secs(9)));
            let response = error.client_visible_upstream_response().unwrap();
            assert_eq!(response.status(), status);
            assert!(!String::from_utf8_lossy(response.body()).contains("synthetic-key"));
            assert!(String::from_utf8_lossy(response.body()).contains("rejected"));
        }
    }
}

// 将多个事件放进同一个 HTTP body，验证解码器缓冲区也遵守取消边界
#[tokio::test]
async fn cancelling_after_first_frame_stops_buffered_frames_and_releases_shared_capacity() {
    for (protocol, url, first) in [
        (
            "openai",
            "/responses",
            "event: response.created\ndata: {\"response\":{\"id\":\"r1\"}}\n\n",
        ),
        (
            "openai-chat",
            "/v1/chat/completions",
            "data: {\"id\":\"r1\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        ),
        (
            "anthropic",
            "/anthropic/v1/messages",
            "event: message_start\ndata: {\"message\":{\"id\":\"r1\"}}\n\n",
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(path(url))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(format!("{first}{first}"), "text/event-stream"),
            )
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let cancellation = CancellationToken::new();
        let mut stream = provider
            .clone()
            .execute(
                request(operation(protocol, true)),
                context(cancellation.clone()),
            )
            .await
            .unwrap();
        stream.next().await.unwrap().unwrap();
        cancellation.cancel();
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(
            error.kind(),
            gateway_core::error::ProviderErrorKind::Cancelled,
            "{protocol}"
        );
        assert_eq!(
            error.send_state(),
            gateway_core::upstream::UpstreamSendState::Sent
        );
        drop(stream);
        let first = provider
            .clone()
            .execute(
                request(operation("openai", true)),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        let second = provider
            .execute(
                request(operation("anthropic", true)),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        drop((first, second));
    }
}

#[tokio::test]
async fn invalid_json_and_missing_sse_terminal_are_never_success() {
    for (protocol, url) in [
        ("openai", "/responses"),
        ("openai-chat", "/v1/chat/completions"),
        ("anthropic", "/anthropic/v1/messages"),
    ] {
        for stream_requested in [false, true] {
            let server = MockServer::start().await;
            let streaming = protocol == "openai" || stream_requested;
            let body = if streaming {
                "data: {invalid json}\n\n"
            } else {
                "{invalid json}"
            };
            Mock::given(path(url))
                .respond_with(ResponseTemplate::new(200).set_body_raw(
                    body,
                    if streaming {
                        "text/event-stream"
                    } else {
                        "application/json"
                    },
                ))
                .mount(&server)
                .await;
            let (provider, _, _) = fixture(&server.uri()).await;
            let mut stream = provider
                .execute(
                    request(operation(protocol, stream_requested)),
                    context(CancellationToken::new()),
                )
                .await
                .unwrap();
            let mut error_seen = false;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(event) => assert!(
                        !event
                            .canonical_facts()
                            .iter()
                            .any(|fact| matches!(fact, GatewayEvent::Completed(_)))
                    ),
                    Err(error) => {
                        assert_eq!(
                            error.kind(),
                            gateway_core::error::ProviderErrorKind::Protocol
                        );
                        error_seen = true;
                        break;
                    }
                }
            }
            assert!(error_seen, "{protocol} streaming={streaming}");
        }
    }
}

#[tokio::test]
async fn all_protocol_deadlines_after_send_release_shared_capacity() {
    for (protocol, url) in [
        ("openai", "/responses"),
        ("openai-chat", "/v1/chat/completions"),
        ("anthropic", "/anthropic/v1/messages"),
    ] {
        let server = MockServer::start().await;
        Mock::given(path(url))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_secs(10))
                    .set_body_json(json!({})),
            )
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let mut stream = provider
            .clone()
            .execute(
                request(operation(protocol, true)),
                crate::support::context_with_budget(
                    CancellationToken::new(),
                    Default::default(),
                    gateway_core::engine::execution::ClientTransport::HttpSse,
                    std::time::Duration::from_secs(3),
                ),
            )
            .await
            .unwrap();
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(
            error.kind(),
            gateway_core::error::ProviderErrorKind::Timeout
        );
        assert_eq!(
            error.send_state(),
            gateway_core::upstream::UpstreamSendState::Ambiguous
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        drop(stream);
        let first = provider
            .clone()
            .execute(
                request(operation("openai", true)),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        let second = provider
            .execute(
                request(operation("anthropic", true)),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        drop((first, second));
    }
}

#[tokio::test]
async fn all_protocol_midstream_errors_keep_atomic_failure_without_success_terminal() {
    for (protocol, url, first, failure) in [
        (
            "openai",
            "/responses",
            "event: response.created\ndata: {\"response\":{\"id\":\"r1\"}}\n\n",
            "event: response.failed\ndata: {\"response\":{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"invalid input\",\"code\":\"synthetic_sse\"}}}\n\n",
        ),
        (
            "openai-chat",
            "/v1/chat/completions",
            "data: {\"id\":\"r1\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
            "data: {\"error\":{\"type\":\"invalid_request_error\",\"message\":\"invalid input\",\"code\":\"synthetic_sse\"}}\n\n",
        ),
        (
            "anthropic",
            "/anthropic/v1/messages",
            "event: message_start\ndata: {\"message\":{\"id\":\"r1\"}}\n\n",
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"invalid input\",\"code\":\"synthetic_sse\"}}\n\n",
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(path(url))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(format!("{first}{failure}"), "text/event-stream"),
            )
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let mut stream = provider
            .execute(
                request(operation(protocol, true)),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        let first_event = stream.next().await.unwrap().unwrap();
        assert!(
            !first_event
                .canonical_facts()
                .iter()
                .any(|fact| matches!(fact, GatewayEvent::Completed(_)))
        );
        let mut error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(
            error.kind(),
            gateway_core::error::ProviderErrorKind::InvalidRequest
        );
        assert_eq!(
            error.send_state(),
            gateway_core::upstream::UpstreamSendState::Sent
        );
        assert_eq!(error.upstream_code().unwrap().as_str(), "synthetic_sse");
        let events = error.take_atomic_client_events();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0]
                .wire_event()
                .unwrap()
                .raw_sse_frame()
                .unwrap()
                .as_ref(),
            failure.as_bytes()
        );
        assert!(
            !events[0]
                .canonical_facts()
                .iter()
                .any(|fact| matches!(fact, GatewayEvent::Completed(_)))
        );
    }
}

#[tokio::test]
async fn malformed_sse_data_cannot_be_hidden_by_a_later_success_terminal() {
    for (protocol, url, terminal) in [
        (
            "openai",
            "/responses",
            "event: response.completed\ndata: {\"response\":{\"id\":\"r1\"}}\n\n",
        ),
        (
            "openai-chat",
            "/v1/chat/completions",
            "data: {\"id\":\"r1\",\"choices\":[{\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        ),
        (
            "anthropic",
            "/anthropic/v1/messages",
            "event: message_start\ndata: {\"message\":{\"id\":\"r1\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(path(url))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!("data: {{malformed}}\n\n{terminal}"),
                "text/event-stream",
            ))
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let mut stream = provider
            .execute(
                request(operation(protocol, true)),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(
            error.kind(),
            gateway_core::error::ProviderErrorKind::Protocol
        );
    }
}

#[tokio::test]
async fn scalar_sse_payloads_and_valid_but_truncated_streams_fail() {
    for (protocol, url, first) in [
        (
            "openai",
            "/responses",
            "event: response.created\ndata: {\"response\":{\"id\":\"r1\"}}\n\n",
        ),
        (
            "openai-chat",
            "/v1/chat/completions",
            "data: {\"id\":\"r1\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
        ),
        (
            "anthropic",
            "/anthropic/v1/messages",
            "event: message_start\ndata: {\"message\":{\"id\":\"r1\"}}\n\n",
        ),
    ] {
        for payload in [
            "data: null\n\n",
            "data: []\n\n",
            "data: true\n\n",
            "data: \"text\"\n\n",
            first,
        ] {
            let server = MockServer::start().await;
            Mock::given(path(url))
                .respond_with(ResponseTemplate::new(200).set_body_raw(payload, "text/event-stream"))
                .mount(&server)
                .await;
            let (provider, _, _) = fixture(&server.uri()).await;
            let mut stream = provider
                .execute(
                    request(operation(protocol, true)),
                    context(CancellationToken::new()),
                )
                .await
                .unwrap();
            if payload == first {
                stream.next().await.unwrap().unwrap();
            }
            let error = stream.next().await.unwrap().unwrap_err();
            assert_eq!(
                error.kind(),
                gateway_core::error::ProviderErrorKind::Protocol
            );
            assert_eq!(
                error.send_state(),
                gateway_core::upstream::UpstreamSendState::Sent
            );
        }
    }
}

#[tokio::test]
async fn invalid_utf8_sse_cannot_be_hidden_by_a_later_success_terminal() {
    for (protocol, url, terminal) in [
        (
            "openai",
            "/responses",
            "event: response.completed\ndata: {\"response\":{\"id\":\"r1\"}}\n\n",
        ),
        (
            "openai-chat",
            "/v1/chat/completions",
            "data: {\"id\":\"r1\",\"choices\":[{\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        ),
        (
            "anthropic",
            "/anthropic/v1/messages",
            "event: message_start\ndata: {\"message\":{\"id\":\"r1\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ),
    ] {
        let server = MockServer::start().await;
        let mut body = b"data: {\"text\":\"".to_vec();
        body.push(0xff);
        body.extend_from_slice(b"\"}\n\n");
        body.extend_from_slice(terminal.as_bytes());
        Mock::given(path(url))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let mut stream = provider
            .execute(
                request(operation(protocol, true)),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(
            error.kind(),
            gateway_core::error::ProviderErrorKind::Protocol
        );
        assert_eq!(
            error.send_state(),
            gateway_core::upstream::UpstreamSendState::Sent
        );
    }
}

#[tokio::test]
async fn utf8_split_across_http_chunks_succeeds_but_incomplete_eof_fails() {
    use std::io::{Read, Write};
    for incomplete in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let first = b"data: {\"id\":\"r1\",\"choices\":[{\"delta\":{\"content\":\"";
        let suffix = b"\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let mut chunks = vec![first.to_vec(), vec![0xe4], vec![0xbd]];
        if !incomplete {
            chunks.extend([vec![0xa0], suffix.to_vec()]);
        }
        let expected = chunks.concat();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0u8; 8192];
            assert!(socket.read(&mut request).unwrap() > 0);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").unwrap();
            for chunk in chunks {
                write!(socket, "{:x}\r\n", chunk.len()).unwrap();
                socket.write_all(&chunk).unwrap();
                socket.write_all(b"\r\n").unwrap();
                socket.flush().unwrap();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            socket.write_all(b"0\r\n\r\n").unwrap();
        });
        let (provider, _, _) = fixture(&format!("http://{address}")).await;
        let mut stream = provider
            .execute(
                request(operation("openai-chat", true)),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        if incomplete {
            let error = stream.next().await.unwrap().unwrap_err();
            assert_eq!(
                error.kind(),
                gateway_core::error::ProviderErrorKind::Protocol
            );
        } else {
            let mut raw = Vec::new();
            let mut completed = 0;
            while let Some(event) = stream.next().await {
                let event = event.unwrap();
                raw.extend_from_slice(event.wire_event().unwrap().raw_sse_frame().unwrap());
                completed += event
                    .canonical_facts()
                    .iter()
                    .filter(|fact| matches!(fact, GatewayEvent::Completed(_)))
                    .count();
            }
            assert_eq!(raw, expected);
            assert_eq!(completed, 1);
        }
        server.join().unwrap();
    }
}

#[tokio::test]
async fn all_accounts_scope_never_selects_an_account_from_another_model_catalog() {
    use gateway_core::{account::*, routing::ProviderKind};
    for status in [200, 500] {
        let matching = MockServer::start().await;
        let other = MockServer::start().await;
        Mock::given(path("/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"deepseek-flash"}]})),
            )
            .mount(&matching)
            .await;
        Mock::given(path("/models"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_json(json!({"data":[{"id":"lifecycle-other-model"}]})),
            )
            .mount(&other)
            .await;
        let (provider, store, matching_id) = crate::support::fixture(&matching.uri()).await;
        let other_id = ProviderAccountId::new("acct_aaa_other").unwrap();
        store
            .create_account(NewProviderAccount {
                account: ProviderAccount::new(
                    other_id.clone(),
                    ProviderKind::new("deepseek").unwrap(),
                    "Other catalog".to_owned(),
                    None,
                    "api_key".to_owned(),
                    CredentialRevision::new(1).unwrap(),
                    None,
                )
                .with_account_facts(
                    true,
                    CredentialState::Ready,
                    QuotaState::unknown(),
                    None,
                    None,
                ),
                model_access: None,
                credential: PlaintextCredential::new(
                    json!({"api_key":"synthetic-other-key","base_url":other.uri()})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            })
            .await
            .unwrap();
        for _ in 0..5 {
            for protocol in ["openai", "openai-chat", "anthropic"] {
                let stream = provider
                    .clone()
                    .execute(
                        crate::support::request_with_accounts(
                            operation(protocol, true),
                            &[matching_id.clone(), other_id.clone()],
                        ),
                        crate::support::round_robin_context(),
                    )
                    .await
                    .unwrap();
                assert_eq!(stream.metadata().provider_account_id(), &matching_id);
                drop(stream);
            }
        }
        assert!(
            other
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.url.path() == "/models")
        );
    }
}

#[tokio::test]
async fn unavailable_catalogs_fail_closed_before_inference() {
    let server = MockServer::start().await;
    Mock::given(path("/models"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let (provider, _, _) = crate::support::fixture(&server.uri()).await;
    for protocol in ["openai", "openai-chat", "anthropic"] {
        let result = provider
            .clone()
            .execute(
                request(operation(protocol, true)),
                context(CancellationToken::new()),
            )
            .await;
        let error = match result {
            Ok(_) => panic!("unverified catalog selected"),
            Err(error) => error,
        };
        assert_eq!(
            error.kind(),
            gateway_core::error::ProviderErrorKind::ProviderInfrastructureUnavailable
        );
        assert_eq!(
            error.send_state(),
            gateway_core::upstream::UpstreamSendState::NotSent
        );
    }
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.url.path() == "/models")
    );
}

#[tokio::test]
async fn catalog_match_does_not_override_disabled_protocol_or_consume_capacity() {
    let server = MockServer::start().await;
    Mock::given(path("/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"deepseek-flash"}]})),
        )
        .mount(&server)
        .await;
    let (provider, _, _) = crate::support::fixture_with_protocols(&server.uri(), &["openai"]).await;
    for protocol in ["openai-chat", "anthropic"] {
        let result = provider
            .clone()
            .execute(
                request(operation(protocol, true)),
                context(CancellationToken::new()),
            )
            .await;
        let error = match result {
            Ok(_) => panic!("disabled protocol selected"),
            Err(error) => error,
        };
        assert_eq!(
            error.kind(),
            gateway_core::error::ProviderErrorKind::NoEligibleAccount
        );
    }
    let first = provider
        .clone()
        .execute(
            request(operation("openai", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    let second = provider
        .execute(
            request(operation("openai", true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    drop((first, second));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn unsupported_model_is_distinct_from_unknown_catalog_and_unauthorized_pool() {
    use gateway_core::{account::*, error::ProviderErrorKind, routing::ProviderKind};
    for other_status in [None, Some(200), Some(503)] {
        let server = MockServer::start().await;
        let other = MockServer::start().await;
        Mock::given(path("/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"data":[{"id":"other-model"}]})),
            )
            .mount(&server)
            .await;
        let (provider, store, id) = crate::support::fixture(&server.uri()).await;
        let mut allowed = vec![id];
        if let Some(status) = other_status {
            Mock::given(path("/models"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .set_body_json(json!({"data":[{"id":"second-other-model"}]})),
                )
                .mount(&other)
                .await;
            let other_id = ProviderAccountId::new("acct_other_catalog").unwrap();
            store
                .create_account(NewProviderAccount {
                    account: ProviderAccount::new(
                        other_id.clone(),
                        ProviderKind::new("deepseek").unwrap(),
                        "Other".to_owned(),
                        None,
                        "api_key".to_owned(),
                        CredentialRevision::new(1).unwrap(),
                        None,
                    )
                    .with_account_facts(
                        true,
                        CredentialState::Ready,
                        QuotaState::unknown(),
                        None,
                        None,
                    ),
                    model_access: None,
                    credential: PlaintextCredential::new(
                        json!({"api_key":"synthetic-other-key","base_url":other.uri()})
                            .as_object()
                            .unwrap()
                            .clone(),
                    ),
                })
                .await
                .unwrap();
            allowed.push(other_id);
        }
        for protocol in ["openai", "openai-chat", "anthropic"] {
            let result = provider
                .clone()
                .execute(
                    crate::support::request_with_accounts(operation(protocol, true), &allowed),
                    context(CancellationToken::new()),
                )
                .await;
            let error = match result {
                Ok(_) => panic!("unsupported model selected"),
                Err(error) => error,
            };
            assert_eq!(
                error.kind(),
                if other_status == Some(503) {
                    ProviderErrorKind::ProviderInfrastructureUnavailable
                } else {
                    ProviderErrorKind::Unsupported
                }
            );
            assert_eq!(
                error.send_state(),
                gateway_core::upstream::UpstreamSendState::NotSent
            );
            // 授权范围中无当前实际账号不能被当作模型不支持
            let result = provider
                .clone()
                .execute(
                    crate::support::request_with_accounts(
                        operation(protocol, true),
                        &[ProviderAccountId::new("acct_unrelated").unwrap()],
                    ),
                    context(CancellationToken::new()),
                )
                .await;
            let error = match result {
                Ok(_) => panic!("unauthorized account selected"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
        }
        assert!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.url.path() == "/models")
        );
        assert!(
            other
                .received_requests()
                .await
                .unwrap()
                .iter()
                .all(|request| request.url.path() == "/models")
        );
    }
}
