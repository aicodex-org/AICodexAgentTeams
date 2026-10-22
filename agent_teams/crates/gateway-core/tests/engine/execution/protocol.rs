//! 验证已知 Jev 协议不匹配在执行入口返回能力错误并记录拒绝

use super::*;

#[test]
fn jev_protocol_mismatch_is_unsupported_before_provider_dispatch() {
    block_on(async {
        let provider = ProviderKind::new("typesafe").unwrap();
        let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new("acct_jev").unwrap(),
            RuntimeAccount::new(provider.clone(), BTreeSet::new()),
        )])));
        let scope = Arc::new(FrozenAccountScope::new(
            directory.clone(),
            ClientRoutingScope::all_accounts(),
        ));
        let snapshot = RuntimeSnapshot::new(
            ConfigRevision::new(1).unwrap(),
            gateway_core::settings::SettingsValues::new(1, 1, "smart", BTreeMap::new(), None, None),
            vec![provider.clone()],
            vec![ProviderModel::new(
                provider,
                UpstreamModelId::new("jev-latest").unwrap(),
                ModelCapabilities::new(BTreeSet::from([OperationKind::StructuredInference]), None)
                    .with_protocols(["typesafe-systemone"]),
            )],
            vec![ClientPolicy::new(
                ClientApiKeyId::new("key_start_test").unwrap(),
                PlaintextClientApiKey::new("sk_start_test").unwrap(),
                scope,
                true,
                RateLimits::unlimited(),
            )],
        )
        .unwrap()
        .with_account_directory(directory);
        let store = Arc::new(TrackingExecutionStore::default());
        let service = DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(snapshot),
            store.clone(),
            ProviderRegistry::default(),
            Arc::new(Admissions::default()),
            Arc::new(UnusedContinuation),
            Arc::new(RecordingClientApiKeyUsage::default()),
            Arc::new(crate::RecordingDiagnostics::default()),
        );
        for (protocol, endpoint) in [
            ("openai", "/v1/responses"),
            ("openai-chat", "/v1/chat/completions"),
        ] {
            let mut input = request(&service, ClientTransport::HttpJson);
            input.public_model = PublicModelId::new("jev-latest").unwrap();
            input.operation = Operation::Generate(GenerateRequest::from_protocol_payload(
                ProtocolPayload::json_object(
                    protocol,
                    serde_json::json!({"model":"jev-latest"})
                        .as_object()
                        .unwrap()
                        .clone(),
                )
                .unwrap(),
            ));
            input.metadata.protocol = protocol.into();
            input.metadata.endpoint = endpoint.into();
            let Err(error) = service.start(input).await else {
                panic!("unsupported protocol must not dispatch");
            };
            assert_eq!(error.kind(), GatewayErrorKind::Unsupported);
        }
        assert_eq!(store.entry_rejections.lock().unwrap().len(), 2);
        assert!(store.requests.lock().unwrap().is_empty());
    });
}
