//! Jev 原生协议与聊天入口的路由错误分类，以及账号隔离和候选回退

use super::*;
use gateway_core::error::RoutingError;
use gateway_core::operation::StructuredInferenceRequest;

fn jev_snapshot(other: Option<ModelCapabilities>) -> RuntimeSnapshot {
    let provider = ProviderKind::new("typesafe").unwrap();
    let mut providers = vec![provider.clone()];
    let mut models = vec![model(
        "typesafe",
        "jev-latest",
        ModelCapabilities::new(BTreeSet::from([OperationKind::StructuredInference]), None)
            .with_protocols(["typesafe-systemone"]),
    )];
    let mut accounts = BTreeMap::from([(
        ProviderAccountId::new("acct_typesafe").unwrap(),
        RuntimeAccount::new(provider.clone(), BTreeSet::new()),
    )]);
    if let Some(caps) = other {
        let other_provider = ProviderKind::new("other").unwrap();
        providers.push(other_provider.clone());
        models.push(model("other", "jev-latest", caps));
        accounts.insert(
            ProviderAccountId::new("acct_other").unwrap(),
            RuntimeAccount::new(other_provider, BTreeSet::new()),
        );
    }
    RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        settings(),
        providers,
        models,
        Vec::new(),
    )
    .unwrap()
    .with_account_directory(Arc::new(RuntimeAccountDirectory::new(accounts)))
    .with_provider_protocols(BTreeMap::from([(
        provider,
        BTreeSet::from(["typesafe-systemone".to_owned()]),
    )]))
}

fn chat(protocol: &str) -> Operation {
    Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            protocol,
            serde_json::json!({"model":"jev-latest"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap(),
    ))
}

#[test]
fn jev_chat_and_responses_report_protocol_mismatch_but_native_routes() {
    let snapshot = jev_snapshot(None);
    let model = PublicModelId::new("jev-latest").unwrap();
    for protocol in ["openai", "openai-chat"] {
        assert_eq!(
            snapshot
                .plan(
                    &model,
                    &chat(protocol),
                    snapshot.all_account_scope(),
                    &RoutingContext::default()
                )
                .unwrap_err(),
            RoutingError::UnsupportedModelProtocol
        );
    }
    let native = Operation::StructuredInference(StructuredInferenceRequest::from_raw_json(
        RawJsonPayload::new("typesafe-systemone", Bytes::from_static(br#"{"model":"jev-latest","state":"ok","questions":{"q":{"type":"noul","instructions":"OK?"}}}"#)).unwrap(),
    ));
    let plan = snapshot
        .plan(
            &model,
            &native,
            snapshot.all_account_scope(),
            &RoutingContext::default(),
        )
        .unwrap();
    assert_eq!(plan.candidates()[0].provider().as_str(), "typesafe");
}

#[test]
fn jev_protocol_diagnosis_does_not_override_blocked_or_empty_scope() {
    let snapshot = jev_snapshot(None);
    let model = PublicModelId::new("jev-latest").unwrap();
    let blocked = RoutingContext {
        blocked_providers: BTreeSet::from([ProviderKind::new("typesafe").unwrap()]),
        ..RoutingContext::default()
    };
    assert!(matches!(
        snapshot.plan(
            &model,
            &chat("openai"),
            snapshot.all_account_scope(),
            &blocked
        ),
        Err(RoutingError::NoCapableProvider { .. })
    ));
    let empty = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::default()),
        ClientRoutingScope::all_accounts(),
    ));
    assert_eq!(
        snapshot
            .plan(&model, &chat("openai"), empty, &RoutingContext::default())
            .unwrap_err(),
        RoutingError::EmptyAccountScope
    );
}

#[test]
fn jev_protocol_diagnosis_preserves_compatible_candidates_and_other_failures() {
    for kind in [OperationKind::Generate, OperationKind::StructuredInference] {
        let snapshot = jev_snapshot(Some(
            ModelCapabilities::new(BTreeSet::from([kind]), None).with_protocols(["openai"]),
        ));
        let result = snapshot.plan(
            &PublicModelId::new("jev-latest").unwrap(),
            &chat("openai"),
            snapshot.all_account_scope(),
            &RoutingContext::default(),
        );
        if kind == OperationKind::Generate {
            assert_eq!(result.unwrap().candidates()[0].provider().as_str(), "other");
        } else {
            assert!(matches!(
                result,
                Err(RoutingError::NoCapableProvider { .. })
            ));
        }
    }
}
