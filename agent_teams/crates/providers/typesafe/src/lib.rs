//! TypeSafe 原生结构化推理的执行与管理能力组装

mod admin;
mod credential;
mod pricing;
mod provider;
mod schema;
mod selection;
mod transport;

use gateway_admin::ports::provider::ProviderAdmin;
use gateway_core::{
    engine::provider::Provider, provider_ports::ProviderStorePorts, task::WorkerContribution,
};
use std::sync::Arc;

pub use credential::{PROTOCOLS, TypeSafeCredential};
pub use provider::TypeSafeProvider;

/// 启动根注册的数据面与管理面能力
pub struct ProviderBundle {
    core_provider: Arc<dyn Provider>,
    admin_provider: Arc<dyn ProviderAdmin>,
}

/// 只创建本地组件，不在进程启动时读取密钥或访问上游
pub async fn initialize(
    ports: ProviderStorePorts,
) -> Result<ProviderBundle, TypeSafeInitializeError> {
    let provider = Arc::new(TypeSafeProvider::new(ports).map_err(|_| TypeSafeInitializeError)?);
    Ok(ProviderBundle {
        core_provider: provider.clone(),
        admin_provider: provider,
    })
}

impl ProviderBundle {
    pub fn core_provider(&self) -> Arc<dyn Provider> {
        self.core_provider.clone()
    }
    pub fn admin_provider(&self) -> Arc<dyn ProviderAdmin> {
        self.admin_provider.clone()
    }
    pub fn take_worker_contributions(&mut self) -> Vec<WorkerContribution> {
        Vec::new()
    }
}

/// 初始化错误不携带连接或凭据内容
#[derive(Debug, thiserror::Error)]
#[error("TypeSafe provider could not initialize")]
pub struct TypeSafeInitializeError;
