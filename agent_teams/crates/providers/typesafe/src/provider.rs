//! TypeSafe 原生结构化推理边界，调度与资源生命周期复用 Core

use crate::{PROTOCOLS, selection::waiting_queue, transport};
use async_trait::async_trait;
use gateway_core::{
    account::ProviderAccountId,
    concurrency::ConcurrencyWaitQueue,
    engine::{
        AttemptContext,
        provider::{Provider, ProviderCallMetadata, ProviderRequest, ProviderStream},
    },
    error::{ProviderError, ProviderErrorKind},
    operation::{Feature, Operation, OperationKind},
    provider_ports::ProviderStorePorts,
    routing::{
        ModelCapabilities, ProviderCatalogGeneration, ProviderKind, ProviderModelCapabilities,
        SupportLevel, UpstreamModelId,
    },
    upstream::UpstreamSendState,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

pub struct TypeSafeProvider {
    pub(crate) ports: ProviderStorePorts,
    pub(crate) kind: ProviderKind,
    pub(crate) waiting: ConcurrencyWaitQueue<ProviderAccountId>,
    catalog_epoch: tokio::time::Instant,
}
impl TypeSafeProvider {
    pub fn new(ports: ProviderStorePorts) -> Result<Self, ProviderError> {
        Ok(Self {
            ports,
            kind: ProviderKind::new("typesafe")
                .map_err(|_| not_sent(ProviderErrorKind::InvalidRequest))?,
            waiting: waiting_queue(),
            catalog_epoch: tokio::time::Instant::now(),
        })
    }
}
pub(crate) fn not_sent(kind: ProviderErrorKind) -> ProviderError {
    ProviderError::new(kind, UpstreamSendState::NotSent)
}
#[async_trait]
impl Provider for TypeSafeProvider {
    fn native_protocols(&self) -> BTreeSet<String> {
        PROTOCOLS.into_iter().map(str::to_owned).collect()
    }

    fn name(&self) -> &str {
        "typesafe"
    }
    fn catalog_generation(&self) -> ProviderCatalogGeneration {
        // 与账号目录的五分钟缓存同周期失效，失败目录不会永久停留在启动快照
        ProviderCatalogGeneration::new(self.catalog_epoch.elapsed().as_secs() / 300 + 1)
    }
    fn model_catalog_is_exhaustive(&self) -> bool {
        false
    }
    fn supported_protocols(&self) -> Option<BTreeSet<String>> {
        Some(PROTOCOLS.into_iter().map(str::to_owned).collect())
    }
    async fn query_model_capabilities(
        &self,
    ) -> Result<Vec<ProviderModelCapabilities>, ProviderError> {
        let accounts = self
            .ports
            .accounts()
            .list_for_provider(&self.kind)
            .await
            .map_err(|_| not_sent(ProviderErrorKind::ProviderInfrastructureUnavailable))?;
        let mut discovered: BTreeMap<UpstreamModelId, BTreeSet<ProviderAccountId>> =
            BTreeMap::new();
        let has_accounts = !accounts.is_empty();
        let mut successful_catalogs = 0;
        for account in accounts {
            let Ok(models) =
                gateway_admin::ports::provider::ProviderAdmin::models(self, account.id(), false)
                    .await
            else {
                continue;
            };
            successful_catalogs += 1;
            for model in models.models {
                discovered
                    .entry(model.id)
                    .or_default()
                    .insert(account.id().clone());
            }
        }
        // 有账号但全数读取失败表示目录未知，不能发布为已知空目录
        // 部分成功保留可验证的账号来源，缺失部分由下一周期或配置发布重试
        if has_accounts && successful_catalogs == 0 {
            return Err(not_sent(
                ProviderErrorKind::ProviderInfrastructureUnavailable,
            ));
        }
        Ok(discovered
            .into_iter()
            .map(|(model, accounts)| {
                let capability = ModelCapabilities::new(
                    BTreeSet::from([OperationKind::StructuredInference]),
                    None,
                )
                .with_protocols(PROTOCOLS)
                .with_feature(Feature::Tools, SupportLevel::Unsupported)
                .with_feature(Feature::Reasoning, SupportLevel::Unsupported)
                .with_feature(Feature::Vision, SupportLevel::Unsupported)
                .with_feature(Feature::NativeContinuation, SupportLevel::Unsupported);
                ProviderModelCapabilities::new(model, capability).with_catalog_accounts(accounts)
            })
            .collect())
    }

    async fn execute(
        self: Arc<Self>,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        // 下游 WebSocket 包装不能绕过当前仅支持 HTTP 的平台边界
        // 使用 Core 冻结的入口事实，在选号和租约获取之前拒绝
        if context.client_transport() == gateway_core::engine::execution::ClientTransport::WebSocket
        {
            return Err(not_sent(ProviderErrorKind::Unsupported));
        }
        let Operation::StructuredInference(generate) = request.operation() else {
            return Err(not_sent(ProviderErrorKind::Unsupported));
        };
        let protocol = generate.payload().protocol().to_owned();
        if request.candidate().provider() != &self.kind || !PROTOCOLS.contains(&protocol.as_str()) {
            return Err(not_sent(ProviderErrorKind::Unsupported));
        }
        let shape = crate::schema::validate_request(generate.payload())?;
        if request
            .candidate()
            .upstream_model()
            .is_none_or(|model| model.as_str() != shape.model)
        {
            return Err(not_sent(ProviderErrorKind::InvalidRequest));
        }
        let selected = tokio::select! {biased; _=context.cancellation().cancelled()=>return Err(not_sent(ProviderErrorKind::Cancelled)),_=context.deadline().wait()=>return Err(not_sent(ProviderErrorKind::Timeout)),result=self.select(request.candidate(),&protocol,&context)=>result?};
        let model = request
            .candidate()
            .upstream_model()
            .ok_or_else(|| not_sent(ProviderErrorKind::InvalidRequest))?
            .clone();
        let account_id = selected.account.id().clone();
        let middleware_context = context.clone();
        let operation = request.operation().clone();
        let requirements = operation.capability_requirements();
        middleware_context
            .execute_middleware(
                operation,
                self.kind.clone(),
                Some(model.as_str().to_owned()),
                account_id,
                Box::new(move |operation, headers| {
                    Box::pin(async move {
                        if operation.capability_requirements() != requirements {
                            return Err(not_sent(ProviderErrorKind::InvalidRequest));
                        }
                        let Operation::StructuredInference(generate) = operation else {
                            return Err(not_sent(ProviderErrorKind::Unsupported));
                        };
                        if generate.payload().protocol() != protocol {
                            return Err(not_sent(ProviderErrorKind::Unsupported));
                        }
                        let metadata = ProviderCallMetadata::new(
                            self.kind.clone(),
                            model.clone(),
                            selected.account.id().clone(),
                            gateway_core::upstream::UpstreamTransport::new("http_json")
                                .map_err(|_| not_sent(ProviderErrorKind::Protocol))?,
                        )
                        .with_account_snapshot(Some(selected.snapshot()));
                        let client = transport::client(selected.account.outbound_proxy())?;
                        let selected = Arc::new(selected);
                        let stream = transport::stream(
                            client,
                            generate,
                            headers,
                            model,
                            selected.clone(),
                            context,
                            self.ports.clone(),
                        )?;
                        Ok(ProviderStream::new(metadata, stream, selected)
                            .with_filtered_account_feedback(
                                self.ports.account_feedback(),
                                |error| {
                                    // 组织限流和调用方参数错误不代表所选 API Key 的健康程度
                                    !matches!(
                                        error.kind(),
                                        ProviderErrorKind::RateLimited
                                            | ProviderErrorKind::InvalidRequest
                                            | ProviderErrorKind::MessageTooBig
                                            | ProviderErrorKind::Unsupported
                                    )
                                },
                            ))
                    })
                }),
            )
            .await
    }
}
