//! 真实 Provider 边界上的问题合同、概率一致性和 JSON 歧义验证
use super::*;
fn native_request() -> Value {
    json!({"model":"jev-latest","state":"text","questions":{
        "n":{"type":"noul","instructions":"Safe?"},
        "c":{"type":"choice","instructions":{"task":"Pick"},"criteria":{"a":"A","b":null}},
        "s":{"type":"score","instructions":["Rate"],"criteria":["bad",{"description":"good"}]}
    }})
}
fn native_response() -> Value {
    json!({"model":"jev-1.14.0","answers":{
        "n":{"type":"noul","noul":0.9},
        "c":{"type":"choice","choice":"a","probabilities":{"a":0.75,"b":0.25},"confidence":0.8},
        "s":{"type":"score","score":0.6,"legend":{"0":"bad","1":{"description":"good"}},"probabilities":{"0":0.4,"1":0.6},"confidence":0.9}
    },"usage":{"input_tokens":12,"output_tokens":0},"future":{"retained":true}})
}
#[tokio::test]
async fn all_native_question_types_and_structured_descriptions_validate() {
    let server = MockServer::start().await;
    Mock::given(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(native_response()))
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let mut stream = provider
        .execute(
            request(operation_raw(
                serde_json::to_vec(&native_request()).unwrap(),
            )),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    let event = stream.next().await.unwrap().unwrap();
    assert!(
        event.canonical_facts().iter().any(
            |fact| matches!(fact,GatewayEvent::Usage(usage) if usage.input_tokens == Some(12))
        )
    );
}
#[tokio::test]
async fn response_probabilities_types_legend_and_expected_score_are_checked() {
    for (pointer, value) in [
        ("/answers/c/probabilities/a", json!(0.74)),
        ("/answers/c/choice", json!("missing")),
        ("/answers/c/confidence", json!(-1)),
        ("/answers/s/score", json!(0.5)),
        ("/answers/s/legend", json!({"0":"bad"})),
        ("/answers/n/type", json!("choice")),
        ("/answers/n/noul", json!(1.01)),
        ("/usage/input_tokens", json!(-1)),
        ("/usage/output_tokens", json!(1.5)),
    ] {
        let server = MockServer::start().await;
        let mut value_response = native_response();
        *value_response.pointer_mut(pointer).unwrap() = value;
        Mock::given(path("/v1/systemone"))
            .respond_with(ResponseTemplate::new(200).set_body_json(value_response))
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let mut stream = provider
            .execute(
                request(operation_raw(
                    serde_json::to_vec(&native_request()).unwrap(),
                )),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        let error = stream.next().await.unwrap().expect_err(pointer);
        assert_eq!(error.send_state(), UpstreamSendState::Sent);
        assert!(error.retry_is_prohibited());
    }
}
#[tokio::test]
async fn field_nesting_limit_does_not_count_protocol_envelope() {
    let nested = |count| (0..count).fold(json!("x"), |value, _| json!([value]));
    let (provider, _, _) = fixture("http://127.0.0.1:1").await;
    let mut value = native_request();
    value["state"] = nested(64);
    let stream = provider
        .clone()
        .execute(
            request(operation_raw(serde_json::to_vec(&value).unwrap())),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    drop(stream);
    value["state"] = nested(65);
    assert!(
        provider
            .execute(
                request(operation_raw(serde_json::to_vec(&value).unwrap())),
                context(CancellationToken::new())
            )
            .await
            .is_err()
    );
}
#[tokio::test]
async fn duplicate_escaped_keys_and_nonfinite_numeric_values_are_rejected() {
    let (provider, _, _) = fixture("http://127.0.0.1:1").await;
    for state in [
        r#"{"key":1,"\u006bey":2}"#,
        r#"{"x":1e999}"#,
        r#"{"x":{"a":1,"a":2}}"#,
    ] {
        let body = format!(
            r#"{{"model":"jev-latest","state":{state},"questions":{{"q":{{"type":"noul","instructions":"x"}}}}}}"#
        );
        assert!(
            provider
                .clone()
                .execute(
                    request(operation_raw(body.into_bytes())),
                    context(CancellationToken::new())
                )
                .await
                .is_err()
        );
    }
    let mut bytes = vec![0xef, 0xbb, 0xbf];
    bytes.extend_from_slice(serde_json::to_string(&native_request()).unwrap().as_bytes());
    assert!(
        provider
            .execute(
                request(operation_raw(bytes)),
                context(CancellationToken::new())
            )
            .await
            .is_err()
    );
}

use base64::Engine as _;

fn image_body() -> Value {
    json!({"model":"jev-latest","state":{"photo":"fixture-image"},
        "assets":{"fixture-image":{"type":"image","format":"png","data_base64":base64::engine::general_purpose::STANDARD.encode(include_bytes!("fixtures/red-circle.png"))}},
        "questions":{"q":{"type":"noul","instructions":"Is the shape red?"}}})
}
fn image_operation(body: &Value, enabled: bool) -> Operation {
    Operation::StructuredInference(StructuredInferenceRequest::from_raw_json(
        RawJsonPayload::new(
            "typesafe-systemone",
            Bytes::from(serde_json::to_vec(body).unwrap()),
        )
        .unwrap()
        .with_context(serde_json::Map::from_iter([(
            "typesafe_images_enabled".to_owned(),
            json!(enabled),
        )])),
    ))
}

#[tokio::test]
async fn images_forward_bytes_and_opt_in_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("typesafe-danger-zone-images", "true"))
        .and(header("authorization", "Bearer synthetic-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response()))
        .expect(1)
        .mount(&server)
        .await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let body = image_body();
    let mut stream = provider
        .execute(
            request(image_operation(&body, true)),
            context(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    assert!(stream.next().await.is_none());
    assert_eq!(
        server.received_requests().await.unwrap()[0].body,
        serde_json::to_vec(&body).unwrap()
    );
}

#[tokio::test]
async fn image_contract_failures_never_send_upstream() {
    let server = MockServer::start().await;
    let (provider, _, _) = fixture(&server.uri()).await;
    let mut cases = vec![(image_body(), false)];
    for (pointer, value) in [
        ("/assets/fixture-image/format", json!("gif")),
        ("/assets/fixture-image/type", json!("video")),
        (
            "/assets/fixture-image/data_base64",
            json!("data:image/png;base64,AAAA"),
        ),
        ("/assets/fixture-image/data_base64", json!("")),
        ("/state/photo", json!("unrelated")),
    ] {
        let mut body = image_body();
        *body.pointer_mut(pointer).unwrap() = value;
        cases.push((body, true));
    }
    let mut multipart = image_body();
    multipart["assets"]["fixture-image"]["part"] = json!("file");
    cases.push((multipart, true));
    let mut oversized = image_body();
    oversized["assets"]["fixture-image"]["data_base64"] =
        json!("A".repeat(16_777_216usize.div_ceil(3) * 4 + 4));
    cases.push((oversized, true));
    for (body, enabled) in cases {
        let error = match provider
            .clone()
            .execute(
                request(image_operation(&body, enabled)),
                context(CancellationToken::new()),
            )
            .await
        {
            Ok(_) => panic!("invalid image accepted"),
            Err(error) => error,
        };
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn images_in_nested_instructions_or_criteria_are_valid() {
    for field in ["instructions", "criteria"] {
        let server = MockServer::start().await;
        Mock::given(path("/v1/systemone"))
            .respond_with(ResponseTemplate::new(200).set_body_json(response()))
            .expect(1)
            .mount(&server)
            .await;
        let (provider, _, _) = fixture(&server.uri()).await;
        let mut body = image_body();
        body["state"] = json!("Evaluate the example");
        body["questions"]["q"][field] = json!({"true":["Look at fixture-image now"]});
        let mut stream = provider
            .execute(
                request(image_operation(&body, true)),
                context(CancellationToken::new()),
            )
            .await
            .unwrap();
        assert!(stream.next().await.unwrap().is_ok());
        assert!(stream.next().await.is_none());
    }
}
