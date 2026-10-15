//! 分组模型映射的精确匹配合同、校验与请求历史快照

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use super::{AccountGroupId, ProviderKind, PublicModelId, UpstreamModelId};
use crate::validation::RoutingError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupModelMappingEntry {
    pub protocol: String,
    pub requested_model: String,
    pub route_model: String,
    pub target_provider: ProviderKind,
}

impl GroupModelMappingEntry {
    pub fn validate(&self) -> Result<(), RoutingError> {
        if !crate::operation::is_known_generate_protocol(&self.protocol) {
            return Err(RoutingError::InvalidModelMapping);
        }
        for model in [&self.requested_model, &self.route_model] {
            if model.trim() != model
                || model.eq_ignore_ascii_case("all")
                || model
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '*' | '?' | ','))
            {
                return Err(RoutingError::InvalidModelMapping);
            }
        }
        PublicModelId::new(self.requested_model.clone())
            .map_err(|_| RoutingError::InvalidModelMapping)?;
        UpstreamModelId::new(self.route_model.clone())
            .map_err(|_| RoutingError::InvalidModelMapping)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupModelMappingPolicy {
    pub id: String,
    pub group_id: AccountGroupId,
    pub enabled: bool,
    pub revision: u64,
    pub mappings: Vec<GroupModelMappingEntry>,
}

impl GroupModelMappingPolicy {
    pub fn validate(&self) -> Result<(), RoutingError> {
        if self.id.is_empty() || self.revision == 0 || self.mappings.is_empty() {
            return Err(RoutingError::InvalidModelMapping);
        }
        let mut seen = BTreeSet::new();
        for entry in &self.mappings {
            entry.validate()?;
            if !seen.insert((&entry.protocol, &entry.requested_model)) {
                return Err(RoutingError::InvalidModelMapping);
            }
        }
        Ok(())
    }
}

/// 保存命中时的策略与分组信息，后续编辑、重命名或删除不改变请求历史
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMappingPolicyRef {
    pub policy_id: String,
    pub policy_revision: u64,
    pub group_id: String,
    pub group_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMappingTrace {
    pub source: String,
    pub protocol: String,
    pub requested_model: String,
    pub route_model: String,
    pub target_provider: Option<String>,
    pub policies: Vec<ModelMappingPolicyRef>,
}
