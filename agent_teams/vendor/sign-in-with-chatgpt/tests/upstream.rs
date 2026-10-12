//! 固定官方提交的全套基线与跨语言错误、模型目录、SSE 输出差分检查
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use super::support::*;
use crate::*;
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

const BASELINE: &str = "0a36fefeb913055c8c7a1a29b63d82b96b2e841a";
fn root() -> PathBuf {
    std::env::var_os("SIWC_UPSTREAM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .join("sign-in-with-chatgpt-devkit")
        })
}
async fn verify_baseline() -> PathBuf {
    let root = root();
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["rev-parse", "HEAD"])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        BASELINE,
        "parity needs the pinned official checkout"
    );
    root
}
#[tokio::test]
async fn official_local_suite_passes_all_57_cases() {
    let root = verify_baseline().await;
    let mut command = tokio::process::Command::new("node");
    command.arg("--test");
    let mut files: Vec<_> = std::fs::read_dir(root.join("packages/local/test"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".test.mjs"))
        .collect();
    files.sort();
    command.args(files);
    let output = command.output().await.unwrap();
    assert!(
        output.status.success(),
        "official tests failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("tests 57"));
    assert!(text.contains("pass 57"));
    assert!(text.contains("fail 0"));
    assert!(text.contains("skipped 0"));
}
#[tokio::test]
async fn rust_outputs_match_official_node_errors_models_and_sse() {
    let root = verify_baseline().await;
    let mut scenarios = Vec::new();
    for status in [200, 400, 401, 403, 422, 429, 503] {
        for body in [
            json!({"error":"invalid_token","error_description":"synthetic private input"}),
            json!({"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"synthetic private input","param":"input"}}),
            json!({"detail":[{"loc":["body","input"],"type":"list_type","input":"synthetic private input","ctx":{"secret":"synthetic"}}]}),
            json!({"error":{"detail":{"error":{"detail":{"code":"unknown_code"}}}}}),
            json!({"code":"unsafe code\n","message":"synthetic private input"}),
            json!({}),
            Value::Null,
        ] {
            scenarios.push(
                json!({"kind":"api","body":body,"status":status,"requestId":"req_synthetic"}),
            );
        }
    }
    for code in [
        "subscription_sharing_user_not_eligible",
        "subscription_sharing_usage_limit_exceeded",
        "subscription_sharing_usage_unavailable",
        "subscription_sharing_unsupported_capability",
        "subscription_sharing_route_not_supported",
        "subscription_sharing_invalid_user",
        "subscription_sharing_user_unavailable",
        "chatpass_v2_scope_not_authorized",
        "chatpass_v2_invalid_authorization_context",
        "invalid_client",
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
        "invalid_token",
        "invalid_api_key",
        "access_denied",
        "model_not_found",
        "subscription_sharing_v2_user_not_eligible",
        "subscription_sharing_v2_route_not_supported",
        "subscription_sharing_v2_invalid_user",
        "subscription_sharing_v2_user_unavailable",
        "subscription_sharing_v2_client_not_enabled",
    ] {
        scenarios.push(json!({"kind":"api","body":{"error":{"code":code,"message":"synthetic private input"}},"status":429,"requestId":"req_synthetic"}));
    }
    for body in [
        json!({"models":[]}),
        json!({"data":[{"id":"not-authoritative"}]}),
        json!({"models":[{"visibility":"hidden"},{"visibility":"list","slug":"real","display_name":"Real"}]}),
        json!({"models":[{"visibility":"list","slug":"","display_name":"Bad"}]}),
    ] {
        scenarios.push(json!({"kind":"models","body":body,"status":200,"headers":{"content-type":"application/json","x-request-id":"req_synthetic"}}));
    }
    let delta = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Café ☕\"}\r\n\r\n";
    let done = "data: {\"type\":\"response.completed\"}\r\n\r\n";
    for wire in [format!("{delta}{done}"),delta.into(),format!("{delta}data: [DONE]\n\n"),"data: {\"type\":\"response.incomplete\"}\n\n".into(),"data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"model_not_found\"}}}\n\n".into(),"data: {\"type\":\"error\",\"code\":\"subscription_sharing_usage_unavailable\"}\n\n".into(),"event: error\ndata: {\"error\":\"invalid_token\"}\n\n".into(),"data: {\"error\":{\"code\":\"invalid_api_key\"}}\n\n".into(),"data: {bad json\n\n".into(),"data: {\"type\":\"response.completed\"}\r\r".into()] {
        scenarios.push(json!({"kind":"sse","wire":wire,"headers":{"content-type":"text/event-stream","x-request-id":"req_synthetic"}}));
    }
    let mut native = Vec::new();
    for scenario in &scenarios {
        if scenario["kind"] == "api" {
            native.push(
                serde_json::to_value(crate::errors::api_error(
                    scenario.get("body"),
                    scenario["status"].as_u64().map(|n| n as u16),
                    scenario["requestId"].as_str(),
                ))
                .unwrap(),
            );
            continue;
        }
        let status = scenario["status"].as_u64().unwrap_or(200) as u16;
        let headers = scenario["headers"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let wire = scenario["wire"]
            .as_str()
            .map(str::as_bytes)
            .map(Vec::from)
            .unwrap_or_else(|| serde_json::to_vec(&scenario["body"]).unwrap());
        let mock = Mock::new(move |_| {
            let mut r = response(status, wire.clone(), &[]);
            r.headers = headers.clone();
            async move { Ok(r) }
        });
        if scenario["kind"] == "models" {
            native.push(
                match crate::responses::list_models(
                    mock.as_ref(),
                    "synthetic-oauth",
                    &CancellationToken::new(),
                )
                .await
                {
                    Ok(v) => serde_json::to_value(v).unwrap(),
                    Err(e) => serde_json::to_value(e).unwrap(),
                },
            );
        } else {
            let options = StreamResponseOptions {
                model: "test-model".into(),
                input: ResponseInput::Text("synthetic input".into()),
                instructions: None,
                signal: CancellationToken::new(),
                on_delta: None,
            };
            native.push(
                match crate::responses::stream_response(
                    mock.as_ref(),
                    "synthetic-oauth",
                    &options,
                    &options.signal,
                )
                .await
                {
                    Ok(v) => serde_json::to_value(v).unwrap(),
                    Err(e) => serde_json::to_value(e).unwrap(),
                },
            );
        }
    }
    let mut child = tokio::process::Command::new("node")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/oracle.mjs"))
        .arg(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&serde_json::to_vec(&scenarios).unwrap())
        .await
        .unwrap();
    drop(stdin);
    let output = child.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let official: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(native.len(), official.len());
    for (i, (rust, node)) in native.iter().zip(&official).enumerate() {
        assert_eq!(rust, node, "case {i}: {}", scenarios[i]);
    }
}

#[tokio::test]
async fn encrypted_state_and_host_identity_roundtrip_through_official_node_store() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let root = verify_baseline().await;
    let f = Fixture::new(offline());
    let saved = saved_state();
    f.write(&saved).await;
    let lock = f.store.lock(&CancellationToken::new()).await.unwrap();
    let host = lock.host_id().await.unwrap();
    lock.finish().unwrap();
    let scenarios = json!([{"kind":"storage","directory":f.directory.path(),"key":STANDARD.encode(f.provider.key)}]);
    let mut child = tokio::process::Command::new("node")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/oracle.mjs"))
        .arg(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(&serde_json::to_vec(&scenarios).unwrap())
        .await
        .unwrap();
    drop(stdin);
    let output = child.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let official: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(official[0]["saved"], saved);
    assert_eq!(official[0]["hostId"], host);
    assert_eq!(f.read().await, saved);
    let lock = f.store.lock(&CancellationToken::new()).await.unwrap();
    assert_eq!(lock.host_id().await.unwrap(), host);
    lock.finish().unwrap();
}
