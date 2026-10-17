//! 网关进程与操作系统能力：配置发现、日志、任务、自更新与 serve/drain

pub mod client_distribution;
mod command_line;
pub mod config;
mod logging;
pub mod official_plugins;
pub mod outbound;
pub mod plugin_distribution;
pub mod pricing;
pub mod process;
pub mod proxy_probe;
pub mod retention;
pub mod serve;
pub mod system_update;
pub mod workers;

use std::sync::Arc;

use axum::Router;
use gateway_admin::ports::{
    client_distribution::ClientDistributionResolver, plugin_release::OfficialPluginReleaseFiles,
    system::SystemOperations,
};
use gateway_core::health::{HealthProbe, WorkerHealthSource};
use gateway_core::lifecycle::CancellationToken;
use gateway_core::lifecycle::ConnectionLifecycle;
use gateway_core::task::{WorkerContribution, WorkerLeaderLeasePort};

pub use config::{ConfigError, HostConfig, LoadableConfig, load_config};

use self::client_distribution::RgAdguardClientDistribution;
use self::logging::{LogGuard, initialize_logging};
use self::serve::{ConnectionTracker, serve_router};
use self::system_update::ProcessSystemOperations;
use self::workers::WorkerSupervisor;

/// Host 初始化的能力集；字段全部私有，不暴露内部监督器或进程状态
pub struct HostBundle {
    config: HostConfig,
    cancellation: CancellationToken,
    connections: Arc<ConnectionTracker>,
    workers: WorkerSupervisor,
    system: Arc<ProcessSystemOperations>,
    client_distribution: Arc<RgAdguardClientDistribution>,
    official_plugins: Arc<official_plugins::FileOfficialPluginRelease>,
    command_signal: Option<command_line::SignalGuard>,
    // 所有宿主能力析构后才排空日志，保留清理阶段的诊断
    log_guard: LogGuard,
}

/// 在启动其他包之前初始化进程级能力
pub async fn initialize(config: HostConfig) -> Result<HostBundle, HostError> {
    let official_plugins = Arc::new(official_plugins::FileOfficialPluginRelease::new(
        config
            .system_update
            .official_plugins_dir()
            .map_err(|_| HostError::OfficialPluginRelease)?,
    ));
    let log_guard = initialize_logging(&config.logging, config.timezone)?;
    let cancellation = CancellationToken::new();
    let connections = Arc::new(ConnectionTracker::new(cancellation.clone()));
    // 关闭请求先排空 HTTP/WS，写泵仍需接收在途请求的最后写入
    // Worker 使用独立信号，只由 drain 结束后的 shutdown 或析构触发
    let workers = WorkerSupervisor::new(CancellationToken::new());
    let system = Arc::new(ProcessSystemOperations::new(
        cancellation.clone(),
        config.system_update.clone(),
    ));
    let client_distribution = Arc::new(RgAdguardClientDistribution::new());
    Ok(HostBundle {
        config,
        log_guard,
        cancellation,
        connections,
        workers,
        system,
        client_distribution,
        official_plugins,
        command_signal: None,
    })
}

/// CLI 的 stdout/stderr 属于命令结果；宿主诊断写入配置目录中的普通文件日志
pub async fn initialize_command_line(mut config: HostConfig) -> Result<HostBundle, HostError> {
    config.logging.stdout = false;
    // CLI 输出属于命令结果，诊断沿用配置的文件目录独立留存
    config.logging.file.enabled = true;
    let mut host = initialize(config).await?;
    host.command_signal = Some(command_line::SignalGuard::start(host.cancellation()));
    Ok(host)
}

impl HostBundle {
    /// 向启动控制台报告一个组装阶段已就绪
    pub fn report_startup_ready(&self, service: &'static str) {
        tracing::info!(target: "gateway_startup", service, "服务启动正常");
    }

    /// 组合根只提交稳定阶段分类，避免错误来源链中的凭据进入普通日志
    pub fn report_startup_failed(&self, stage: &'static str, category: &'static str) {
        tracing::error!(target: "gateway_startup", event = "startup.failed", stage, category, "服务初始化失败，启动已终止");
    }

    /// 命令失败与服务启动失败区分，日志只保留安全的阶段分类
    pub fn report_command_failed(&self, stage: &'static str, category: &'static str) {
        tracing::error!(target: "gateway_command", event = "command.failed", stage, category, "插件命令失败，命令已终止");
    }

    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    #[must_use]
    pub fn system_operations(&self) -> Arc<dyn SystemOperations> {
        self.system.clone()
    }

    #[must_use]
    /// 返回惰性下载解析能力；网络请求只会在管理 API 调用时发生
    pub fn client_distribution_resolver(&self) -> Arc<dyn ClientDistributionResolver> {
        self.client_distribution.clone()
    }

    /// 返回与当前可执行文件一同部署的只读官方插件发行目录
    #[must_use]
    pub fn official_plugin_release_files(&self) -> Arc<dyn OfficialPluginReleaseFiles> {
        self.official_plugins.clone()
    }

    /// 身份来自构建脚本写入二进制的常量，不采信运行时配置或环境覆盖
    #[must_use]
    pub fn official_plugin_release_identity(
        &self,
    ) -> gateway_admin::model::plugins::official::OfficialPluginReleaseIdentity {
        gateway_admin::model::plugins::official::OfficialPluginReleaseIdentity {
            gateway_version: env!("AICODEX_AGENT_TEAMS_VERSION").to_owned(),
            gateway_git_sha: env!("AICODEX_AGENT_TEAMS_GIT_SHA").to_owned(),
        }
    }

    #[must_use]
    pub fn proxy_probe<E>(
        &self,
        build_client: impl Fn(reqwest::ClientBuilder) -> Result<reqwest::Client, E>
        + Send
        + Sync
        + 'static,
    ) -> Arc<dyn gateway_admin::ports::proxy::ProxyProbe> {
        Arc::new(proxy_probe::HttpProxyProbe::default().with_client_builder(build_client))
    }

    #[must_use]
    pub fn connection_lifecycle(&self) -> Arc<dyn ConnectionLifecycle> {
        self.connections.clone()
    }

    /// 报告文件日志写入或归档失败，避免将缺失日志误报为完整
    #[must_use]
    pub fn logging_health_probe(&self) -> Arc<dyn HealthProbe> {
        self.log_guard.health_probe()
    }

    #[must_use]
    pub fn worker_health(&self) -> Arc<dyn WorkerHealthSource> {
        self.workers.health_source()
    }

    pub fn start_workers(
        &self,
        plan: Vec<WorkerContribution>,
        lease: Arc<dyn WorkerLeaderLeasePort>,
        diagnostics: Arc<dyn gateway_core::diagnostics::OperationalDiagnostics>,
    ) -> Result<(), HostError> {
        self.workers.start(plan, lease, diagnostics)?;
        Ok(())
    }

    /// 进程唯一阻塞点；停止 HTTP 监听后完成 worker join
    pub fn serve(
        &mut self,
        router: Router,
    ) -> impl std::future::Future<Output = Result<(), HostError>> + '_ {
        let config = &self.config;
        let workers = &self.workers;
        let connections = &self.connections;
        // 在创建 future 时安装守卫，未被 poll 的服务 future 被丢弃时也要取消公共能力
        let cancellation = ServeCancellation {
            connections,
            workers,
            completed: false,
        };
        async move {
            let result = serve_router(
                router,
                &config.listen.host,
                config.listen.port,
                connections.cancellation(),
                Arc::clone(connections),
            )
            .await;
            connections.begin_draining();
            workers.shutdown(config.worker_shutdown_timeout()).await;
            cancellation.complete();
            match &result {
                Ok(()) => {
                    tracing::info!(target: "gateway_shutdown", pid = std::process::id(), "HTTP 监听已关闭，后台任务已结束")
                }
                Err(error) => {
                    tracing::error!(target: "gateway_shutdown", pid = std::process::id(), %error, "HTTP 服务异常停止，后台任务已清理")
                }
            }
            result?;
            Ok(())
        }
    }
}

/// 借用式 serve 被取消时，Host 仍可能被调用方保留，不能依赖 Host 析构关闭公共能力
struct ServeCancellation<'a> {
    connections: &'a ConnectionTracker,
    workers: &'a WorkerSupervisor,
    completed: bool,
}

impl ServeCancellation<'_> {
    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for ServeCancellation<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.connections.begin_draining();
            self.workers.abort();
            tracing::warn!(target: "gateway_shutdown", event = "serve.cancelled", "HTTP 服务等待已取消，连接注册已停止，已请求终止后台任务");
        }
    }
}

impl Drop for HostBundle {
    fn drop(&mut self) {
        // CLI 与启动失败也要通知公共能力停止
        // WorkerSupervisor 自己负责取消并终止剩余任务
        self.cancellation.cancel();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Logging(#[from] logging::LogError),
    #[error(transparent)]
    Workers(#[from] workers::WorkerStartError),
    #[error(transparent)]
    Serve(#[from] serve::ServeError),
    #[error("官方插件发行目录不可用")]
    OfficialPluginRelease,
}
