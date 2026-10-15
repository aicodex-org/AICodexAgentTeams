//! 分组模型映射策略的管理合同与原子变更命令

use super::{PageSize, Revision};
use chrono::{DateTime, Utc};
use gateway_core::routing::GroupModelMappingPolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMappingRecord {
    pub policy: GroupModelMappingPolicy,
    pub group_name: String,
    pub remark: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ModelMappingListQuery {
    pub page: u32,
    pub page_size: PageSize,
    pub search: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ModelMappingPage {
    pub config_revision: Revision,
    pub items: Vec<ModelMappingRecord>,
    pub total: u64,
    pub page: u32,
    pub page_size: u16,
}

#[derive(Debug, Clone)]
pub struct SaveModelMapping {
    pub policy: GroupModelMappingPolicy,
    pub remark: Option<String>,
    /// 创建时为 None，更新时必须匹配当前策略版本
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ModelMappingMutation {
    pub id: String,
    pub record: Option<ModelMappingRecord>,
    pub config_revision: Revision,
}

pub fn validate_save(command: &SaveModelMapping) -> Result<(), super::AdminError> {
    command.policy.validate().map_err(|_| {
        super::AdminError::invalid(
            "映射必须指定协议、请求模型 ID、路由模型 ID 和目标平台，且请求模型不能重复或使用通配符",
        )
    })?;
    if command
        .remark
        .as_deref()
        .is_some_and(|s| s.len() > 4096 || s.chars().any(char::is_control))
        || command
            .expected_revision
            .is_some_and(|revision| revision == 0 || revision >= i64::MAX as u64)
    {
        return Err(super::AdminError::invalid("映射备注或策略版本不合法"));
    }
    Ok(())
}

/// 仅编译配置候选，不占用账号租约或发送上游请求
#[derive(Debug, Clone)]
pub struct ModelMappingPreview {
    pub config_revision: u64,
    pub model_mapping: Option<gateway_core::routing::ModelMappingTrace>,
    pub targets: Vec<(String, String)>,
}
