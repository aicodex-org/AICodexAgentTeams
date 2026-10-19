//! 网关唯一组合根：按启动模式连接各 Bundle

mod command;
mod config;
mod plugins;
mod server;
mod startup;

pub use command::{PluginCommand, plugin_command};
pub use config::GatewayConfig;
pub use server::run;

/// 组合根只保留包级错误分类，不展开内部实现或敏感配置
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    #[error(transparent)]
    Worker(#[from] gateway_core::task::WorkerDefinitionError),
    #[error(transparent)]
    PluginCommand(#[from] gateway_plugin_runtime::PluginCommandError),
    #[error(transparent)]
    Config(#[from] gateway_host::ConfigError),
    #[error(transparent)]
    Host(#[from] gateway_host::HostError),
    #[error(transparent)]
    Outbound(#[from] gateway_host::outbound::HttpError),
    #[error(transparent)]
    Store(#[from] gateway_store::StoreError),
    #[error(transparent)]
    CommandStoreDrain(#[from] gateway_store::CommandStoreDrainError),
    #[error(transparent)]
    OpenAi(#[from] provider_openai::OpenAiInitializeError),
    #[error(transparent)]
    DeepSeek(#[from] provider_deepseek::DeepSeekInitializeError),
    #[error(transparent)]
    MiMo(#[from] provider_mimo::MiMoInitializeError),
    #[error(transparent)]
    Xai(#[from] provider_xai::XaiInitializeError),
    #[error(transparent)]
    Registry(#[from] gateway_core::engine::provider::RegistryError),
    #[error(transparent)]
    Core(#[from] gateway_core::CoreError),
    #[error(transparent)]
    Admin(#[from] gateway_admin::model::AdminError),
    #[error(transparent)]
    Api(#[from] gateway_api::ApiError),
}

impl BootstrapError {
    /// 从现有类型化事实选择安全分类，不序列化底层 message 或来源
    pub(super) fn category(&self) -> &'static str {
        use gateway_store::{StoreBackend, StoreError};
        match self {
            Self::Store(StoreError::Unavailable {
                backend: StoreBackend::PostgreSql,
                ..
            }) => "postgres_unavailable",
            Self::Store(StoreError::Unavailable {
                backend: StoreBackend::Redis,
                ..
            }) => "redis_unavailable",
            Self::Store(StoreError::InvalidData { .. }) => "store_invalid_data",
            Self::Store(StoreError::Conflict { .. }) => "store_conflict",
            Self::Store(StoreError::NotFound { .. }) => "store_not_found",
            _ => "operation_failed",
        }
    }

    /// 仅返回稳定包级分类，不展开底层错误或配置内容
    pub(super) fn stage(&self) -> &'static str {
        match self {
            Self::Worker(_) => "workers",
            Self::PluginCommand(_) => "plugin_command",
            Self::Config(_) => "configuration",
            Self::Host(_) => "host",
            Self::Outbound(_) => "outbound",
            Self::Store(_) | Self::CommandStoreDrain(_) => "store",
            Self::OpenAi(_) => "provider_openai",
            Self::DeepSeek(_) => "provider_deepseek",
            Self::MiMo(_) => "provider_mimo",
            Self::Xai(_) => "provider_xai",
            Self::Registry(_) => "provider_registry",
            Self::Core(_) => "core",
            Self::Admin(_) => "admin",
            Self::Api(_) => "api",
        }
    }
}
