//! 验证模型映射从请求执行到 Provider 派发、失败记录与续接账号隔离

use super::*;
use gateway_core::routing::{GroupModelMappingEntry, GroupModelMappingPolicy};

#[derive(Default)]
struct MappedProvider {
    calls: AtomicUsize,
    expect_other_account: AtomicBool,
}

#[async_trait]
impl Provider for MappedProvider {
    fn name(&self) -> &str {
        "openai"
    }
    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        ProviderCatalogGeneration::default()
    }
    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        Ok(Vec::new())
    }
    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.operation().protocol(), "openai");
        assert_eq!(
            request.candidate().upstream_model().unwrap().as_str(),
            "gpt-route"
        );
        for scope in [
            request.candidate().account_scope().as_ref(),
            context.account_scope().unwrap().as_ref(),
        ] {
            assert!(scope.allows(&ProviderAccountId::new("acct_openai").unwrap()));
            assert_eq!(
                scope.allows(&ProviderAccountId::new("acct_other").unwrap()),
                self.expect_other_account.load(Ordering::SeqCst),
            );
        }
        Arc::new(ProcessingProvider).execute(request, context).await
    }
}

fn mapped_service(
    target: &str,
    continuation: Arc<dyn NativeContinuationPort>,
) -> (
    DefaultExecutionService,
    Arc<MappedProvider>,
    Arc<TrackingExecutionStore>,
) {
    mapped_service_with_extensions(target, continuation, None)
}

fn mapped_service_with_extensions(
    target: &str,
    continuation: Arc<dyn NativeContinuationPort>,
    extensions: Option<ExtensionSetReference>,
) -> (
    DefaultExecutionService,
    Arc<MappedProvider>,
    Arc<TrackingExecutionStore>,
) {
    let kind = ProviderKind::new("openai").unwrap();
    let group_a = AccountGroupId::new(format!("grp_{:032x}", 1)).unwrap();
    let group_b = AccountGroupId::new(format!("grp_{:032x}", 2)).unwrap();
    let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([
        (
            ProviderAccountId::new("acct_openai").unwrap(),
            RuntimeAccount::new(kind.clone(), BTreeSet::from([group_a.clone()])),
        ),
        (
            ProviderAccountId::new("acct_other").unwrap(),
            RuntimeAccount::new(kind.clone(), BTreeSet::from([group_b.clone()])),
        ),
    ])));
    let scope = Arc::new(FrozenAccountScope::new(
        directory.clone(),
        ClientRoutingScope::restricted(
            vec![
                RoutingGroupSnapshot::new(group_a.clone(), "mapped group".into()),
                RoutingGroupSnapshot::new(group_b.clone(), "other group".into()),
            ],
            BTreeSet::from([group_a.clone(), group_b]),
            BTreeSet::from([kind.clone()]),
        )
        .unwrap(),
    ));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(1, 1, "smart", BTreeMap::new(), None, None),
        vec![kind.clone()],
        ["gpt-start", "gpt-route"]
            .into_iter()
            .map(|model| {
                ProviderModel::new(
                    kind.clone(),
                    UpstreamModelId::new(model).unwrap(),
                    ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None),
                )
            })
            .collect(),
        vec![ClientPolicy::new(
            ClientApiKeyId::new("key_start_test").unwrap(),
            PlaintextClientApiKey::new("sk_start_test").unwrap(),
            scope,
            true,
            RateLimits::unlimited(),
        )],
    )
    .unwrap()
    .with_account_directory(directory)
    .with_native_provider_protocols(BTreeMap::from([(
        kind.clone(),
        BTreeSet::from(["openai".into()]),
    )]))
    .with_group_model_mapping_policies(vec![GroupModelMappingPolicy {
        id: "policy_dispatch".into(),
        group_id: group_a,
        enabled: true,
        revision: 3,
        mappings: vec![
            GroupModelMappingEntry {
                protocol: "openai".into(),
                requested_model: "gpt-start".into(),
                route_model: target.into(),
                target_provider: kind.clone(),
            },
            GroupModelMappingEntry {
                protocol: "openai".into(),
                requested_model: "gpt-route".into(),
                route_model: "gpt-start".into(),
                target_provider: kind,
            },
        ],
    }])
    .unwrap();
    let provider = Arc::new(MappedProvider::default());
    let store = Arc::new(TrackingExecutionStore::default());
    (
        DefaultExecutionService::new(
            RuntimeSnapshotHandle::new(snapshot.with_extensions(extensions)),
            store.clone(),
            ProviderRegistry::new([provider.clone() as Arc<dyn Provider>]).unwrap(),
            Arc::new(Admissions::default()),
            continuation,
            Arc::new(RecordingClientApiKeyUsage::default()),
            Arc::new(crate::RecordingDiagnostics::default()),
        ),
        provider,
        store,
    )
}

#[test]
fn mapped_request_dispatches_exact_target_and_persists_mapping_for_each_transport() {
    block_on(async {
        for transport in [
            ClientTransport::HttpJson,
            ClientTransport::HttpSse,
            ClientTransport::WebSocket,
        ] {
            let (service, provider, store) =
                mapped_service("gpt-route", Arc::new(UnusedContinuation));
            let mut started = service.start(request(&service, transport)).await.unwrap();
            started.session.collect_uncommitted().await.unwrap();
            started.session.commit_downstream(Some(200)).await.unwrap();
            assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
            let requests = store.requests.lock().unwrap();
            let trace = requests[0]
                .model_mapping
                .as_ref()
                .expect("persisted mapping");
            assert_eq!(trace.source, "group");
            assert_eq!(trace.requested_model, "gpt-start");
            assert_eq!(trace.route_model, "gpt-route");
            assert_eq!(trace.policies[0].group_name, "mapped group");
            assert_eq!(trace.policies[0].policy_revision, 3);
        }
    });
}

#[test]
fn invalid_target_is_recorded_without_dispatching_original_model() {
    block_on(async {
        let (service, provider, store) =
            mapped_service("missing-model", Arc::new(UnusedContinuation));
        assert!(
            matches!(service.start(request(&service, ClientTransport::HttpJson)).await, Err(error) if error.kind() == GatewayErrorKind::NoAvailableProvider)
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        let rejections = store.entry_rejections.lock().unwrap();
        assert_eq!(rejections.len(), 1);
        assert_eq!(
            rejections[0].model_mapping.as_ref().unwrap().route_model,
            "missing-model"
        );
    });
}

struct OtherGroupContinuation;
impl NativeContinuationPort for OtherGroupContinuation {
    fn resolve<'a>(
        &'a self,
        key: &'a ClientApiKeyId,
        previous: &'a PreviousResponseId,
    ) -> BoxFuture<'a, Result<Option<NativeContinuationPin>, NativeContinuationStoreError>> {
        Box::pin(async move {
            Ok(Some(NativeContinuationPin::new(
                previous.clone(),
                previous.clone(),
                key.clone(),
                ProviderKind::new("openai").unwrap(),
                ProviderAccountId::new("acct_other").unwrap(),
            )))
        })
    }
    fn record<'a>(
        &'a self,
        _: NativeContinuationPin,
    ) -> BoxFuture<'a, Result<(), NativeContinuationStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn same_provider_continuation_cannot_restore_an_account_outside_mapping_group() {
    block_on(async {
        let (service, provider, store) =
            mapped_service("gpt-route", Arc::new(OtherGroupContinuation));
        let mut input = request(&service, ClientTransport::HttpJson);
        input.metadata.previous_response_id = Some(PreviousResponseId::new("previous-response"));
        assert!(
            matches!(service.start(input).await, Err(error) if error.kind() == GatewayErrorKind::PolicyDenied)
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert!(
            store.entry_rejections.lock().unwrap()[0]
                .model_mapping
                .is_some()
        );
    });
}

#[test]
fn model_route_plugin_cannot_change_a_frozen_group_mapping_or_trigger_a_second_rule() {
    block_on(async {
        for (decision, allowed) in [
            (ModelRouteDecision::Unhandled, true),
            (
                ModelRouteDecision::Route {
                    provider: None,
                    model: Some(PublicModelId::new("gpt-start").unwrap()),
                },
                true,
            ),
            (
                ModelRouteDecision::Route {
                    provider: Some(ProviderKind::new("openai").unwrap()),
                    model: Some(PublicModelId::new("gpt-route").unwrap()),
                },
                true,
            ),
            (
                ModelRouteDecision::Route {
                    provider: None,
                    model: Some(PublicModelId::new("other-model").unwrap()),
                },
                false,
            ),
            (
                ModelRouteDecision::Route {
                    provider: Some(ProviderKind::new("deepseek").unwrap()),
                    model: None,
                },
                false,
            ),
            (ModelRouteDecision::Reject, false),
        ] {
            let generation = super::super::extensions::reference("group-mapping-routing-plugin");
            let index = RequestPolicyExtensionIndex::default();
            let policy = Arc::new(FixedRoutingPolicy {
                decision,
                inputs: Mutex::new(Vec::new()),
            });
            let _owner = index
                .register(generation.id().clone(), policy.clone())
                .unwrap();
            let (service, provider, store) = mapped_service_with_extensions(
                "gpt-route",
                Arc::new(UnusedContinuation),
                Some(generation),
            );
            let service = service.with_request_policies(index);
            let result = service
                .start(request(&service, ClientTransport::HttpJson))
                .await;
            if allowed {
                let mut started = result.unwrap();
                started.session.collect_uncommitted().await.unwrap();
                started.session.commit_downstream(Some(200)).await.unwrap();
                assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
                let requests = store.requests.lock().unwrap();
                let trace = requests[0].model_mapping.as_ref().unwrap();
                assert_eq!(trace.requested_model, "gpt-start");
                assert_eq!(trace.route_model, "gpt-route");
            } else {
                assert!(
                    matches!(result, Err(error) if error.kind() == GatewayErrorKind::PolicyDenied)
                );
                assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
                let rejections = store.entry_rejections.lock().unwrap();
                let trace = rejections[0].model_mapping.as_ref().unwrap();
                assert_eq!(trace.requested_model, "gpt-start");
                assert_eq!(trace.route_model, "gpt-route");
            }
            let inputs = policy.inputs.lock().unwrap();
            assert_eq!(inputs.len(), 1);
            assert_eq!(
                inputs[0].available_providers(),
                &BTreeSet::from([ProviderKind::new("openai").unwrap()])
            );
        }
    });
}

#[test]
fn model_route_plugin_keeps_existing_rename_behavior_when_no_group_mapping_matches() {
    block_on(async {
        let kind = ProviderKind::new("openai").unwrap();
        let generation = super::super::extensions::reference("unmatched-group-routing-plugin");
        let index = RequestPolicyExtensionIndex::default();
        let policy = Arc::new(FixedRoutingPolicy {
            decision: ModelRouteDecision::Route {
                provider: Some(kind.clone()),
                model: Some(PublicModelId::new("gpt-route").unwrap()),
            },
            inputs: Mutex::new(Vec::new()),
        });
        let _owner = index.register(generation.id().clone(), policy).unwrap();
        let (service, provider, store) = mapped_service_with_extensions(
            "gpt-route",
            Arc::new(UnusedContinuation),
            Some(generation),
        );
        provider.expect_other_account.store(true, Ordering::SeqCst);
        let service = service.with_request_policies(index);
        let mut input = request(&service, ClientTransport::HttpJson);
        input.public_model = PublicModelId::new("unmatched-input").unwrap();
        input.operation = start_operation_for_model("unmatched-input");
        let mut started = service.start(input).await.unwrap();
        started.session.collect_uncommitted().await.unwrap();
        started.session.commit_downstream(Some(200)).await.unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(store.requests.lock().unwrap()[0].model_mapping.is_none());
        assert_eq!(store.upstream_models(), vec![Some("gpt-route".into())]);
    });
}
