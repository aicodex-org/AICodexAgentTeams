//! 分组模型映射的校验、持久化与运行快照发布

use crate::{
    model::{AdminError, MutationContext, model_mappings::*},
    ports::model_mappings::ModelMappingStore,
};
use async_trait::async_trait;
use gateway_core::{
    routing::GroupModelMappingPolicy,
    runtime::{RuntimeSnapshotHandle, SnapshotControl},
};
use std::sync::Arc;

#[async_trait]
pub trait ModelMappingService: Send + Sync {
    async fn list(&self, query: ModelMappingListQuery) -> Result<ModelMappingPage, AdminError>;
    async fn get(&self, id: &str) -> Result<ModelMappingRecord, AdminError>;
    async fn save(
        &self,
        context: &MutationContext,
        command: SaveModelMapping,
    ) -> Result<ModelMappingMutation, AdminError>;
    async fn set_enabled(
        &self,
        context: &MutationContext,
        id: &str,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<ModelMappingMutation, AdminError>;
    async fn delete(
        &self,
        context: &MutationContext,
        id: &str,
        expected_revision: u64,
    ) -> Result<ModelMappingMutation, AdminError>;
    fn preview(
        &self,
        policy: GroupModelMappingPolicy,
        key_id: gateway_core::policy::ClientApiKeyId,
        protocol: String,
        model: String,
    ) -> Result<ModelMappingPreview, AdminError>;
    fn targets(
        &self,
        group: &gateway_core::routing::AccountGroupId,
    ) -> Result<Vec<(String, gateway_core::routing::ProviderKind)>, AdminError>;
    fn validate(&self, policy: &GroupModelMappingPolicy) -> Result<(), AdminError>;
}

pub(crate) struct DefaultModelMappingService {
    pub store: Arc<dyn ModelMappingStore>,
    pub snapshot: Arc<dyn SnapshotControl>,
    pub published: RuntimeSnapshotHandle,
}

impl DefaultModelMappingService {
    async fn publish(
        &self,
        result: crate::ports::store::AdminStoreResult<ModelMappingMutation>,
    ) -> Result<ModelMappingMutation, AdminError> {
        let result = result.map_err(|error| super::map_store_error(error, "model mapping"))?;
        super::publish_committed(self.snapshot.as_ref(), result.config_revision).await?;
        Ok(result)
    }
}

#[async_trait]
impl ModelMappingService for DefaultModelMappingService {
    async fn list(&self, query: ModelMappingListQuery) -> Result<ModelMappingPage, AdminError> {
        if query.page == 0
            || query
                .search
                .as_deref()
                .is_some_and(|s| s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(AdminError::invalid("模型映射查询参数不合法"));
        }
        self.store
            .list(query)
            .await
            .map_err(|e| super::map_store_error(e, "model mapping list"))
    }
    async fn get(&self, id: &str) -> Result<ModelMappingRecord, AdminError> {
        self.store
            .get(id)
            .await
            .map_err(|e| super::map_store_error(e, "model mapping detail"))
    }
    async fn save(
        &self,
        context: &MutationContext,
        mut command: SaveModelMapping,
    ) -> Result<ModelMappingMutation, AdminError> {
        if command.expected_revision.is_none() {
            command.policy.id = format!("mapping_{}", uuid::Uuid::now_v7().simple());
        }
        validate_save(&command)?;
        self.validate(&command.policy)?;
        self.publish(self.store.save(command, context).await).await
    }
    async fn set_enabled(
        &self,
        context: &MutationContext,
        id: &str,
        expected_revision: u64,
        enabled: bool,
    ) -> Result<ModelMappingMutation, AdminError> {
        let record = self.get(id).await?;
        let mut policy = record.policy;
        policy.enabled = enabled;
        let command = SaveModelMapping {
            policy,
            remark: record.remark,
            expected_revision: Some(expected_revision),
        };
        validate_save(&command)?;
        // 停用必须能在 Provider 已下线时执行，启用则重新校验当前协议能力
        if enabled {
            self.validate(&command.policy)?;
        }
        self.publish(self.store.save(command, context).await).await
    }
    async fn delete(
        &self,
        context: &MutationContext,
        id: &str,
        expected_revision: u64,
    ) -> Result<ModelMappingMutation, AdminError> {
        if expected_revision == 0 {
            return Err(AdminError::invalid("必须指定策略版本"));
        }
        self.publish(self.store.delete(id, expected_revision, context).await)
            .await
    }
    fn preview(
        &self,
        policy: GroupModelMappingPolicy,
        key_id: gateway_core::policy::ClientApiKeyId,
        protocol: String,
        model: String,
    ) -> Result<ModelMappingPreview, AdminError> {
        use gateway_core::{
            operation::{GenerateRequest, Operation, ProtocolPayload},
            routing::{PublicModelId, RoutingContext},
        };
        if !gateway_core::operation::is_known_generate_protocol(&protocol) {
            return Err(AdminError::invalid("请求协议不合法"));
        }
        let current = self
            .published
            .snapshot_for_diagnostics()
            .ok_or_else(|| AdminError::unavailable("运行配置暂不可用"))?;
        current
            .validate_group_model_mapping_policy(&policy)
            .map_err(|e| AdminError::invalid("策略目标不满足同协议映射要求").with_source(e))?;
        let mut policies = current
            .group_model_mapping_policies()
            .filter(|p| p.group_id != policy.group_id)
            .cloned()
            .collect::<Vec<_>>();
        policies.push(policy);
        let snapshot = current
            .as_ref()
            .clone()
            .with_group_model_mapping_policies(policies)
            .map_err(|e| AdminError::invalid("策略配置冲突").with_source(e))?;
        let key = snapshot
            .client_policy(&key_id)
            .filter(|key| key.enabled())
            .ok_or_else(|| AdminError::not_found("可用的 API 密钥不存在"))?;
        let model =
            PublicModelId::new(model).map_err(|_| AdminError::invalid("请求模型 ID 不合法"))?;
        let mut body = serde_json::Map::new();
        body.insert(
            "model".into(),
            serde_json::Value::String(model.as_str().into()),
        );
        let payload = ProtocolPayload::json_object(protocol, body)
            .map_err(|_| AdminError::invalid("请求协议不合法"))?;
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(payload));
        let plan = snapshot
            .plan(
                &model,
                &operation,
                key.account_scope().clone(),
                &RoutingContext::default(),
            )
            .map_err(|e| {
                AdminError::invalid("映射冲突或当前分组没有符合协议与模型限制的路由目标")
                    .with_source(e)
            })?;
        Ok(ModelMappingPreview {
            config_revision: snapshot.revision().get(),
            model_mapping: plan.model_mapping().cloned(),
            targets: plan
                .candidates()
                .iter()
                .map(|candidate| {
                    (
                        candidate.provider().to_string(),
                        candidate
                            .upstream_model()
                            .map_or_else(String::new, |m| m.to_string()),
                    )
                })
                .collect(),
        })
    }
    fn targets(
        &self,
        group: &gateway_core::routing::AccountGroupId,
    ) -> Result<Vec<(String, gateway_core::routing::ProviderKind)>, AdminError> {
        Ok(self
            .published
            .snapshot_for_diagnostics()
            .ok_or_else(|| AdminError::unavailable("运行配置暂不可用"))?
            .group_model_mapping_targets(group))
    }
    fn validate(&self, policy: &GroupModelMappingPolicy) -> Result<(), AdminError> {
        self.published
            .snapshot_for_diagnostics()
            .ok_or_else(|| AdminError::unavailable("运行配置暂不可用，无法校验同协议映射"))?
            .validate_group_model_mapping_policy(policy)
            .map_err(|e| {
                AdminError::invalid("目标平台不支持指定的原生协议，或映射与现有模型别名冲突")
                    .with_source(e)
            })
    }
}
