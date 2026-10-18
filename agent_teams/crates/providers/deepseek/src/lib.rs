//! DeepSeek 单账号三协议的原生执行与管理能力组装

mod admin;
mod credential;
mod pricing;
mod pricing_schedule;
mod provider;
mod selection;
mod transport;

use gateway_admin::ports::provider::ProviderAdmin;
use gateway_core::{
    engine::provider::Provider, provider_ports::ProviderStorePorts, task::WorkerContribution,
};
use std::sync::Arc;

pub use credential::{DeepSeekCredential, PROTOCOLS};
pub use provider::DeepSeekProvider;

/// 启动根注册的数据面与管理面能力
pub struct ProviderBundle {
    core_provider: Arc<dyn Provider>,
    admin_provider: Arc<dyn ProviderAdmin>,
}

/// 只创建本地组件，不在进程启动时读取密钥或访问上游
pub async fn initialize(
    ports: ProviderStorePorts,
) -> Result<ProviderBundle, DeepSeekInitializeError> {
    let provider = Arc::new(DeepSeekProvider::new(ports).map_err(|_| DeepSeekInitializeError)?);
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
#[error("DeepSeek provider could not initialize")]
pub struct DeepSeekInitializeError;
