//! 分组模型映射持久化窄端口

use super::store::AdminStoreResult;
use crate::model::{MutationContext, model_mappings::*};
use async_trait::async_trait;

#[async_trait]
pub trait ModelMappingStore: Send + Sync {
    async fn list(&self, query: ModelMappingListQuery) -> AdminStoreResult<ModelMappingPage>;
    async fn get(&self, id: &str) -> AdminStoreResult<ModelMappingRecord>;
    async fn save(
        &self,
        command: SaveModelMapping,
        context: &MutationContext,
    ) -> AdminStoreResult<ModelMappingMutation>;
    async fn delete(
        &self,
        id: &str,
        expected_revision: u64,
        context: &MutationContext,
    ) -> AdminStoreResult<ModelMappingMutation>;
}
