//! 执行扩展索引的代次隔离、计划保活与租约替身

use std::sync::Arc;

use gateway_core::engine::extensions::{ExecutionExtensionIndex, ExecutionExtensionPlans};
use gateway_core::engine::upstream_adapter::{UpstreamAdapter, UpstreamAdapterPlan};
use gateway_core::routing::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference};
use gateway_core::{
    engine::AttemptContext, error::ProviderError, identity::ProviderKind, routing::UpstreamModelId,
};

struct ReadyLease;

impl ExtensionSetLease for ReadyLease {
    fn is_ready(&self) -> bool {
        true
    }
}

pub(super) fn reference(id: &str) -> ExtensionSetReference {
    let id = ExtensionSetId::new(id.into()).unwrap();
    ExtensionSetReference::new(id, Arc::new(ReadyLease))
}

#[derive(Debug)]
struct EmptyAdapterPlan;

impl UpstreamAdapterPlan for EmptyAdapterPlan {
    fn select(
        &self,
        _: &AttemptContext,
        _: &ProviderKind,
        _: &UpstreamModelId,
    ) -> Result<Option<Arc<dyn UpstreamAdapter>>, ProviderError> {
        Ok(None)
    }
}

struct PublishedPlans(Arc<ExecutionExtensionPlans>);

impl ExtensionSetLease for PublishedPlans {
    fn is_ready(&self) -> bool {
        self.0.upstream_adapters.is_some()
    }
}

#[test]
fn adapter_only_generation_stays_alive_until_the_last_frozen_plan_is_released() {
    let index = ExecutionExtensionIndex::default();
    let id = ExtensionSetId::new("adapter-only".into()).unwrap();
    let plans = Arc::new(ExecutionExtensionPlans {
        middleware: None,
        upstream_adapters: Some(Arc::new(EmptyAdapterPlan)),
    });
    let weak_plans = Arc::downgrade(&plans);
    let published = Arc::new(PublishedPlans(index.register(id.clone(), plans).unwrap()));
    let weak_publication = Arc::downgrade(&published);
    let reference = ExtensionSetReference::new(id.clone(), published);
    assert!(index.middleware(&reference).is_none());
    let frozen = index.upstream_adapters(&reference).unwrap();
    drop(reference);

    // 当前发布可以退场；在途计划仍保活整个代次，不能覆盖相同身份
    assert!(weak_publication.upgrade().is_some());
    assert!(weak_plans.upgrade().is_some());
    assert!(index.register(id.clone(), Arc::default()).is_err());
    let in_flight = frozen.clone();
    drop(frozen);
    assert!(weak_publication.upgrade().is_some());
    drop(in_flight);

    assert!(weak_publication.upgrade().is_none());
    assert!(weak_plans.upgrade().is_none());
    let released = ExtensionSetReference::new(id.clone(), Arc::new(ReadyLease));
    assert!(index.upstream_adapters(&released).is_none());
    let _replacement = index.register(id, Arc::default()).unwrap();
}

#[test]
fn execution_index_does_not_own_registered_generations() {
    let index = ExecutionExtensionIndex::default();
    let reference = reference("unpublished");
    let plans = Arc::new(ExecutionExtensionPlans::default());
    let weak = Arc::downgrade(&plans);
    let owner = index.register(reference.id().clone(), plans).unwrap();
    assert!(weak.upgrade().is_some());
    drop(owner);
    assert!(weak.upgrade().is_none());
    assert!(index.middleware(&reference).is_none());
    assert!(index.upstream_adapters(&reference).is_none());
}

#[derive(Debug)]
struct SelectedAdapter;
impl UpstreamAdapter for SelectedAdapter {
    fn execute(
        self: Arc<Self>,
        _: gateway_core::engine::upstream_adapter::UpstreamAdapterInvocation,
    ) -> gateway_core::engine::provider::EventStream {
        panic!("strict native mapping must reject before executing the adapter")
    }
    fn transport(&self) -> &str {
        "test-adapter"
    }
}
#[derive(Debug)]
struct SelectedAdapterPlan;
impl UpstreamAdapterPlan for SelectedAdapterPlan {
    fn select(
        &self,
        _: &AttemptContext,
        _: &ProviderKind,
        _: &UpstreamModelId,
    ) -> Result<Option<Arc<dyn UpstreamAdapter>>, ProviderError> {
        Ok(Some(Arc::new(SelectedAdapter)))
    }
}

#[test]
fn strict_native_mapping_rejects_selected_adapters_without_disabling_other_native_calls() {
    use gateway_core::{
        account::{AccountSelectionPolicy, RotationStrategy},
        engine::{
            AccountAttemptContext, ModelRequestId, RequestAttemptContext,
            upstream_adapter::FrozenUpstreamAdapterPlan,
        },
        error::ProviderErrorKind,
        lifecycle::{CancellationToken, Deadline},
        policy::ClientApiKeyId,
        upstream::UpstreamSendState,
    };
    for (selected, strict) in [(true, true), (false, true), (true, false)] {
        let adapter: Arc<dyn UpstreamAdapterPlan> = if selected {
            Arc::new(SelectedAdapterPlan)
        } else {
            Arc::new(EmptyAdapterPlan)
        };
        let context = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new("req_native_mapping").unwrap(),
                ClientApiKeyId::new("key_native_mapping").unwrap(),
            )
            .with_required_native_protocol(strict)
            .with_upstream_adapters(Some(FrozenUpstreamAdapterPlan::new(
                adapter,
                reference("native-mapping"),
            ))),
            std::num::NonZeroU32::new(1).unwrap(),
            Deadline::default(),
            AccountSelectionPolicy::new(
                RotationStrategy::Smart,
                std::num::NonZeroU32::new(1).unwrap(),
                std::time::Duration::ZERO,
            ),
            AccountAttemptContext::default(),
            None,
            CancellationToken::new(),
        );
        let result = context.upstream_adapter(
            &ProviderKind::new("openai").unwrap(),
            &UpstreamModelId::new("model").unwrap(),
        );
        if selected && strict {
            let error = result.unwrap_err();
            assert_eq!(error.kind(), ProviderErrorKind::Unsupported);
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
            assert!(error.retry_is_prohibited());
        } else {
            assert_eq!(result.unwrap().is_some(), selected);
        }
    }
}
