//! 验证分组精确模型映射的协议、账号隔离、冲突和目录投影

use super::*;
use gateway_core::routing::{GroupModelMappingEntry, GroupModelMappingPolicy};
use gateway_core::validation::RoutingError;

fn group(n: u32) -> AccountGroupId {
    AccountGroupId::new(format!("grp_{n:032x}")).unwrap()
}
fn provider(name: &str) -> ProviderKind {
    ProviderKind::new(name).unwrap()
}
fn entry(protocol: &str, target: &str, model: &str) -> GroupModelMappingEntry {
    GroupModelMappingEntry {
        protocol: protocol.into(),
        requested_model: "alias".into(),
        route_model: model.into(),
        target_provider: provider(target),
    }
}
fn policy(n: u32, mappings: Vec<GroupModelMappingEntry>) -> GroupModelMappingPolicy {
    GroupModelMappingPolicy {
        id: format!("policy_{n}"),
        group_id: group(n),
        enabled: true,
        revision: 2,
        mappings,
    }
}
fn fixture(
    policies: Vec<GroupModelMappingPolicy>,
    bound: &[u32],
) -> (RuntimeSnapshot, Arc<FrozenAccountScope>) {
    let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([
        (
            ProviderAccountId::new("acct_a").unwrap(),
            RuntimeAccount::new(provider("deepseek"), BTreeSet::from([group(1)])),
        ),
        (
            ProviderAccountId::new("acct_b").unwrap(),
            RuntimeAccount::new(provider("deepseek"), BTreeSet::from([group(2)])),
        ),
        (
            ProviderAccountId::new("acct_c").unwrap(),
            RuntimeAccount::new(provider("openai"), BTreeSet::from([group(1)])),
        ),
    ])));
    let scope = if bound.is_empty() {
        ClientRoutingScope::all_accounts()
    } else {
        let ids = bound.iter().map(|n| group(*n)).collect();
        ClientRoutingScope::restricted(
            bound
                .iter()
                .map(|n| RoutingGroupSnapshot::new(group(*n), format!("group {n}")))
                .collect(),
            ids,
            BTreeSet::from([provider("deepseek"), provider("openai")]),
        )
        .unwrap()
    };
    let scope = Arc::new(FrozenAccountScope::new(directory.clone(), scope));
    let values = gateway_core::settings::SettingsValues::new(
        3,
        50,
        "smart",
        BTreeMap::from([
            ("alias".into(), "fallback".into()),
            ("target".into(), "fallback".into()),
        ]),
        None,
        None,
    );
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(4).unwrap(),
        values,
        vec![provider("deepseek"), provider("openai")],
        vec![
            model(
                "deepseek",
                "target",
                capabilities().with_protocols(["openai", "openai-chat"]),
            ),
            model("openai", "fallback", capabilities()),
        ],
        vec![],
    )
    .unwrap()
    .with_account_directory(directory)
    .with_native_provider_protocols(BTreeMap::from([
        (
            provider("deepseek"),
            BTreeSet::from(["openai".into(), "openai-chat".into()]),
        ),
        (provider("openai"), BTreeSet::from(["openai".into()])),
    ]))
    .with_group_model_mapping_policies(policies)
    .unwrap();
    (snapshot, scope)
}
fn plan(
    snapshot: &RuntimeSnapshot,
    scope: Arc<FrozenAccountScope>,
) -> Result<gateway_core::routing::RoutingPlan, RoutingError> {
    snapshot.plan(
        &PublicModelId::new("alias").unwrap(),
        &operation(),
        scope,
        &RoutingContext::default(),
    )
}

#[test]
fn exact_mapping_is_one_step_and_keeps_only_matching_group_and_provider() {
    let (snapshot, scope) = fixture(
        vec![policy(1, vec![entry("openai", "deepseek", "target")])],
        &[1, 2],
    );
    let plan = plan(&snapshot, scope).unwrap();
    assert_eq!(plan.candidates().len(), 1);
    let candidate = &plan.candidates()[0];
    assert_eq!(candidate.provider().as_str(), "deepseek");
    assert_eq!(candidate.upstream_model().unwrap().as_str(), "target");
    assert!(
        candidate
            .account_scope()
            .allows(&ProviderAccountId::new("acct_a").unwrap())
    );
    assert!(
        !candidate
            .account_scope()
            .allows(&ProviderAccountId::new("acct_b").unwrap())
    );
    assert!(
        !candidate
            .account_scope()
            .allows(&ProviderAccountId::new("acct_c").unwrap())
    );
    let trace = plan.model_mapping().unwrap();
    assert_eq!(trace.policies[0].group_name, "group 1");
    assert_eq!(trace.policies[0].policy_revision, 2);
    assert_eq!(trace.route_model, "target");
}

#[test]
fn protocol_mismatch_and_unbound_keys_preserve_global_routing() {
    for (protocol, bound) in [("openai-chat", vec![1]), ("openai", vec![])] {
        let (snapshot, scope) = fixture(
            vec![policy(1, vec![entry(protocol, "deepseek", "target")])],
            &bound,
        );
        let plan = plan(&snapshot, scope).unwrap();
        assert_eq!(plan.model_mapping().unwrap().source, "global");
        assert_eq!(
            plan.candidates()[0].upstream_model().unwrap().as_str(),
            "fallback"
        );
    }
}

#[test]
fn equivalent_targets_union_only_matching_groups() {
    let (snapshot, scope) = fixture(
        vec![
            policy(1, vec![entry("openai", "deepseek", "target")]),
            policy(2, vec![entry("openai", "deepseek", "target")]),
        ],
        &[1, 2],
    );
    let plan = plan(&snapshot, scope).unwrap();
    assert_eq!(plan.model_mapping().unwrap().policies.len(), 2);
    for id in ["acct_a", "acct_b"] {
        assert!(
            plan.account_scope()
                .allows(&ProviderAccountId::new(id).unwrap())
        );
    }
}

#[test]
fn conflicting_targets_fail_before_availability_and_never_fallback() {
    let (snapshot, scope) = fixture(
        vec![
            policy(1, vec![entry("openai", "deepseek", "missing")]),
            policy(2, vec![entry("openai", "openai", "fallback")]),
        ],
        &[1, 2],
    );
    assert!(matches!(
        plan(&snapshot, scope),
        Err(RoutingError::ModelMappingConflict)
    ));
}

#[test]
fn empty_group_intersection_never_restores_parent_pool() {
    let (snapshot, scope) = fixture(
        vec![policy(3, vec![entry("openai", "deepseek", "target")])],
        &[1, 3],
    );
    assert!(matches!(
        plan(&snapshot, scope),
        Err(RoutingError::EmptyAccountScope)
    ));
}

#[test]
fn unavailable_mapped_model_does_not_use_global_alias() {
    let (snapshot, scope) = fixture(
        vec![policy(1, vec![entry("openai", "deepseek", "missing")])],
        &[1],
    );
    assert!(matches!(
        plan(&snapshot, scope),
        Err(RoutingError::NoCapableProvider { .. })
    ));
}

#[test]
fn adapted_or_unknown_protocol_fails_closed_at_save_and_execution() {
    let mapping = policy(1, vec![entry("openai", "deepseek", "target")]);
    let (snapshot, scope) = fixture(vec![mapping.clone()], &[1]);
    let snapshot = snapshot.with_native_provider_protocols(BTreeMap::new());
    assert_eq!(
        snapshot.validate_group_model_mapping_policy(&mapping),
        Err(RoutingError::ModelMappingProtocolMismatch)
    );
    assert!(matches!(
        plan(&snapshot, scope),
        Err(RoutingError::ModelMappingProtocolMismatch)
    ));
}

#[test]
fn duplicate_protocol_model_and_wildcards_are_rejected() {
    let valid = entry("openai", "deepseek", "target");
    for value in ["", "all", "ALL", "*", "gpt-*", "a,b", " target"] {
        let mut invalid = valid.clone();
        invalid.route_model = value.into();
        assert!(invalid.validate().is_err(), "{value}");
        let mut invalid = valid.clone();
        invalid.requested_model = value.into();
        assert!(invalid.validate().is_err(), "{value}");
    }
    assert!(
        policy(1, vec![valid.clone(), valid.clone()])
            .validate()
            .is_err()
    );
    assert!(
        policy(1, vec![valid, entry("openai-chat", "deepseek", "target")])
            .validate()
            .is_ok()
    );
}

#[test]
fn protocol_catalog_points_to_only_matching_provider_and_target() {
    let (snapshot, scope) = fixture(
        vec![policy(1, vec![entry("openai-chat", "deepseek", "target")])],
        &[1, 2],
    );
    let alias = PublicModelId::new("alias").unwrap();
    assert_eq!(
        snapshot.mapped_model_for_protocol_provider(
            &alias,
            &provider("deepseek"),
            "openai-chat",
            &scope
        ),
        Some("target".into())
    );
    assert_eq!(
        snapshot.mapped_model_for_protocol_provider(
            &alias,
            &provider("openai"),
            "openai-chat",
            &scope
        ),
        None
    );
    assert_eq!(
        snapshot.mapped_model_for_protocol_provider(&alias, &provider("openai"), "openai", &scope),
        Some("fallback".into())
    );
    assert!(snapshot.public_models_for_scope(&scope).contains(&alias));
}

#[test]
fn disabled_strategy_restores_previous_global_mapping() {
    let mut mapping = policy(1, vec![entry("openai", "deepseek", "target")]);
    mapping.enabled = false;
    let (snapshot, scope) = fixture(vec![mapping], &[1]);
    let plan = plan(&snapshot, scope).unwrap();
    assert_eq!(plan.model_mapping().unwrap().source, "global");
    assert_eq!(
        plan.candidates()[0].upstream_model().unwrap().as_str(),
        "fallback"
    );
}

#[test]
fn mapping_cannot_bypass_target_account_model_policy_or_fixed_continuation_provider() {
    let (snapshot, scope) = fixture(
        vec![policy(1, vec![entry("openai", "deepseek", "target")])],
        &[1, 2],
    );
    let result = snapshot.plan(
        &PublicModelId::new("alias").unwrap(),
        &operation(),
        scope,
        &RoutingContext {
            required_provider: Some(provider("openai")),
            ..RoutingContext::default()
        },
    );
    assert!(matches!(
        result,
        Err(RoutingError::NoCapableProvider { .. })
    ));
    let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
        ProviderAccountId::new("acct_denied").unwrap(),
        RuntimeAccount::new(provider("deepseek"), BTreeSet::from([group(1)])).with_model_access(
            gateway_core::account::AccountModelAccess::new(
                gateway_core::account::AccountModelAccessMode::Denylist,
                vec!["target".into()],
            )
            .unwrap(),
        ),
    )])));
    let scope = Arc::new(FrozenAccountScope::new(
        directory,
        ClientRoutingScope::restricted(
            vec![RoutingGroupSnapshot::new(group(1), "group 1".into())],
            BTreeSet::from([group(1)]),
            BTreeSet::from([provider("deepseek")]),
        )
        .unwrap(),
    ));
    assert!(matches!(
        plan(&snapshot, scope),
        Err(RoutingError::NoCapableProvider { .. })
    ));
}

#[test]
fn frozen_plan_preserves_mapping_history_after_strategy_update() {
    let (snapshot, scope) = fixture(
        vec![policy(1, vec![entry("openai", "deepseek", "target")])],
        &[1],
    );
    let original = plan(&snapshot, scope.clone()).unwrap();
    let updated = snapshot
        .with_group_model_mapping_policies(vec![policy(
            1,
            vec![entry("openai", "openai", "fallback")],
        )])
        .unwrap();
    assert_eq!(
        plan(&updated, scope)
            .unwrap()
            .model_mapping()
            .unwrap()
            .route_model,
        "fallback"
    );
    assert_eq!(original.model_mapping().unwrap().route_model, "target");
    let serialized = serde_json::to_value(original.model_mapping().unwrap()).unwrap();
    assert_eq!(serialized["source"], "group");
    assert_eq!(serialized["requestedModel"], "alias");
    assert_eq!(serialized["policies"][0]["groupName"], "group 1");
}

#[test]
fn global_trace_records_model_change_without_pinning_provider() {
    let (snapshot, scope) = fixture(Vec::new(), &[1]);
    let plan = plan(&snapshot, scope).unwrap();
    let trace = plan.model_mapping().unwrap();
    assert_eq!(trace.source, "global");
    assert_eq!(trace.route_model, "fallback");
    assert!(trace.target_provider.is_none());
    assert!(trace.policies.is_empty());
}
