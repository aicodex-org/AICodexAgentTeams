//! 按 Provider 分派凭据管理服务，组合授权与导入流程

mod flow;

use std::sync::Arc;

use gateway_core::{routing::ProviderKind, runtime::SnapshotControl};

use crate::{
    model::AdminError,
    ports::{provider::ProviderAdminRegistry, proxy::ProxyStore, store::AccountStore},
};

pub use flow::ProviderCredentials;

/// 所有 Provider 共用凭据用例；每次操作从已发布目录冻结自己的管理实现
pub struct CredentialsService {
    providers: ProviderAdminRegistry,
    accounts: Arc<dyn AccountStore>,
    proxies: Arc<dyn ProxyStore>,
    snapshot: Arc<dyn SnapshotControl>,
}

impl CredentialsService {
    #[must_use]
    pub fn new(
        providers: ProviderAdminRegistry,
        accounts: Arc<dyn AccountStore>,
        proxies: Arc<dyn ProxyStore>,
        snapshot: Arc<dyn SnapshotControl>,
    ) -> Self {
        Self {
            providers,
            accounts,
            proxies,
            snapshot,
        }
    }

    /// 连接编辑按已保存账号分派，避免客户端入口猜测 Provider
    pub async fn for_account(
        &self,
        account_id: &gateway_core::account::ProviderAccountId,
    ) -> Result<ProviderCredentials, AdminError> {
        let details = self
            .accounts
            .credential_details_by_id(account_id)
            .await
            .map_err(|error| super::map_store_error(error, "provider credential"))?
            .ok_or_else(|| AdminError::not_found("Provider 凭据不存在"))?;
        self.for_provider(&details.credential.provider_kind)
    }

    pub fn for_provider(&self, kind: &ProviderKind) -> Result<ProviderCredentials, AdminError> {
        let provider = self
            .providers
            .require(kind)
            .map_err(|error| super::map_provider_error(error, "provider credentials"))?;
        Ok(ProviderCredentials::new(
            provider,
            self.accounts.clone(),
            self.proxies.clone(),
            self.snapshot.clone(),
        ))
    }
}
