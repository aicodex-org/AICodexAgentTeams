//! MiMo 原生 Provider 执行边界，发送前选号并交由现有中间件和 Core 生命周期协调

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

pub struct MiMoProvider {
    pub(crate) ports: ProviderStorePorts,
    pub(crate) kind: ProviderKind,
    pub(crate) waiting: ConcurrencyWaitQueue<ProviderAccountId>,
    catalog_epoch: tokio::time::Instant,
}
impl MiMoProvider {
    pub fn new(ports: ProviderStorePorts) -> Result<Self, ProviderError> {
        Ok(Self {
            ports,
            kind: ProviderKind::new("mimo")
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
impl Provider for MiMoProvider {
    fn native_protocols(&self) -> BTreeSet<String> {
        PROTOCOLS.into_iter().map(str::to_owned).collect()
    }

    fn name(&self) -> &str {
        "mimo"
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
                let capability =
                    ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None)
                        .with_protocols(PROTOCOLS)
                        .with_upstream_feature_validation()
                        .with_feature(Feature::Tools, SupportLevel::Native)
                        .with_feature(Feature::Reasoning, SupportLevel::Native)
                        .with_feature(Feature::Vision, SupportLevel::Unknown)
                        .with_feature(Feature::JsonSchema, SupportLevel::Unknown)
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
        let Operation::Generate(generate) = request.operation() else {
            return Err(not_sent(ProviderErrorKind::Unsupported));
        };
        let protocol = generate.protocol_payload().protocol().to_owned();
        if request.candidate().provider() != &self.kind || !PROTOCOLS.contains(&protocol.as_str()) {
            return Err(not_sent(ProviderErrorKind::Unsupported));
        }
        if crate::transport::has_unsupported_stateful_fields(generate) {
            return Err(not_sent(ProviderErrorKind::Unsupported));
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
                        let Operation::Generate(generate) = operation else {
                            return Err(not_sent(ProviderErrorKind::Unsupported));
                        };
                        if generate.protocol_payload().protocol() != protocol {
                            return Err(not_sent(ProviderErrorKind::Unsupported));
                        }
                        let metadata = ProviderCallMetadata::new(
                            self.kind.clone(),
                            model.clone(),
                            selected.account.id().clone(),
                            transport::upstream_transport(&generate)?,
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
                            .with_account_feedback(self.ports.account_feedback()))
                    })
                }),
            )
            .await
    }
}
