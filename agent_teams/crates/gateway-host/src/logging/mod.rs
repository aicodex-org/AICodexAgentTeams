//! 进程结构化日志与按日期/大小轮转的文件 writer

use std::env;
use std::io;
use std::sync::Arc;

use gateway_core::health::HealthProbe;
use std::time::Instant;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::{FilterExt as _, filter_fn};
use tracing_subscriber::{Layer as _, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::config::LoggingConfig;

mod health;
mod panic;
mod sink;
mod writer;

use health::{LogHealth, LoggingHealth};
use sink::{LogOutput, LogSink, SHUTDOWN_TIMEOUT, SinkGuard};
use writer::RotatingLogWriter;

// 文件前缀统一为 AICodexAgentTeams-<kebab-case 类别>；专用 tracing target 使用 snake_case
const APPLICATION_LOG_FILE_PREFIX: &str = "AICodexAgentTeams-application";

/// 内部分类标识；由 host.logging.oauth_recovery 控制是否写入独立文件
const OAUTH_RECOVERY_LOG_TARGET: &str = "oauth_recovery";
const OAUTH_RECOVERY_LOG_FILE_PREFIX: &str = "AICodexAgentTeams-oauth-recovery";

/// 请求转储含完整凭据与正文，只允许在显式开启时写入独立文件
const REQUEST_DUMP_LOG_TARGET: &str = "request_dump";
const REQUEST_DUMP_LOG_FILE_PREFIX: &str = "AICodexAgentTeams-request-dump";

/// 所有输出共用关闭预算，正常退出排空，永久阻塞时明确报告缺口
pub struct LogGuard {
    outputs: Vec<SinkGuard>,
    health: Arc<LoggingHealth>,
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("log IO failed")]
    Io(#[from] io::Error),
    #[error("logging filter is invalid")]
    InvalidFilter,
    #[error("global tracing subscriber is already initialized")]
    AlreadyInitialized,
    #[error("logging size limit is too large")]
    SizeOverflow,
}

/// 初始化日志；文件按完整日期留存，不以分片数量淘汰窗口内记录
pub fn initialize_logging(
    config: &LoggingConfig,
    timezone: gateway_core::time::DeploymentTimeZone,
) -> Result<LogGuard, LogError> {
    let directive = env::var("RUST_LOG").unwrap_or_else(|_| config.level.clone());
    let file_filter = application_file_filter(&directive)?;
    let recovery_file_filter = oauth_recovery_file_filter();
    let request_dump_file_filter = request_dump_file_filter();
    let stdout_filter =
        EnvFilter::try_new(stdout_filter_directive(&directive, config.file.enabled))
            .map_err(|_| LogError::InvalidFilter)?;
    let mut guards = Vec::new();
    let mut health = Vec::new();

    let stdout_writer = if config.stdout {
        let output_health = LogHealth::new("stdout");
        let (writer, guard) = LogSink::spawn(
            LogOutput::Stdout(io::stdout()),
            "gateway-log-stdout",
            Arc::clone(&output_health),
        )?;
        guards.push(guard);
        health.push(output_health);
        Some(writer)
    } else {
        None
    };
    let file_writer = if config.file.enabled {
        let output_health = LogHealth::new("application");
        let (writer, guard) = create_file_writer(
            config,
            timezone,
            config.file.retention_days,
            APPLICATION_LOG_FILE_PREFIX,
            "gateway-log-application-file",
            Arc::clone(&output_health),
        )?;
        guards.push(guard);
        health.push(output_health);
        Some(writer)
    } else {
        None
    };
    let recovery_file_writer = if config.oauth_recovery {
        let output_health = LogHealth::new("oauth_recovery");
        let (writer, guard) = create_file_writer(
            config,
            timezone,
            config.file.retention_days,
            OAUTH_RECOVERY_LOG_FILE_PREFIX,
            "gateway-log-oauth-recovery-file",
            Arc::clone(&output_health),
        )?;
        guards.push(guard);
        health.push(output_health);
        Some(writer)
    } else {
        None
    };
    let request_dump_file_writer = if config.request_dump {
        let output_health = LogHealth::new("request_dump");
        let (writer, guard) = create_file_writer(
            config,
            timezone,
            config.request_dump_retention_days,
            REQUEST_DUMP_LOG_FILE_PREFIX,
            "gateway-log-request-dump-file",
            Arc::clone(&output_health),
        )?;
        guards.push(guard);
        health.push(output_health);
        Some(writer)
    } else {
        None
    };

    let stdout_layer = stdout_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .compact()
            .with_timer(DeploymentClock(timezone))
            .with_writer(writer)
            .with_target(false)
            .with_ansi(false)
            .with_filter(stdout_filter.and(filter_fn(is_public_metadata)))
    });
    let file_layer = file_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_timer(DeploymentClock(timezone))
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_filter(file_filter.and(filter_fn(is_public_metadata)))
    });
    let recovery_file_layer = recovery_file_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_timer(DeploymentClock(timezone))
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_filter(recovery_file_filter.and(filter_fn(|metadata| {
                metadata.is_span() || metadata.target() == OAUTH_RECOVERY_LOG_TARGET
            })))
    });
    let request_dump_file_layer = request_dump_file_writer.map(|writer| {
        tracing_subscriber::fmt::layer()
            .json()
            .with_timer(DeploymentClock(timezone))
            .with_writer(writer)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_current_span(true)
            .with_span_list(true)
            .with_filter(request_dump_file_filter.and(filter_fn(|metadata| {
                metadata.is_span() || metadata.target() == REQUEST_DUMP_LOG_TARGET
            })))
    });
    let oauth_recovery_enabled = config.oauth_recovery;
    let request_dump_enabled = config.request_dump;
    tracing_subscriber::registry()
        .with(stdout_layer)
        .with(file_layer)
        .with(recovery_file_layer)
        .with(request_dump_file_layer)
        // enabled! 是采集前的探测，不能只依赖各输出层的过滤结果
        // 全局门禁仅处理敏感 target，开关之外的动态指令没有采集授权
        .with(filter_fn(move |metadata| match metadata.target() {
            OAUTH_RECOVERY_LOG_TARGET => oauth_recovery_enabled,
            REQUEST_DUMP_LOG_TARGET => request_dump_enabled,
            _ => is_public_metadata(metadata),
        }))
        .try_init()
        .map_err(|_| LogError::AlreadyInitialized)?;
    panic::install();
    Ok(LogGuard {
        outputs: guards,
        health: Arc::new(LoggingHealth(health)),
    })
}

// 敏感边界独立于 EnvFilter；动态 span 和更具体的环境指令均不能重新启用
fn is_public_metadata(metadata: &tracing::Metadata<'_>) -> bool {
    ![OAUTH_RECOVERY_LOG_TARGET, REQUEST_DUMP_LOG_TARGET]
        .iter()
        .any(|target| {
            metadata.target() == *target
                || metadata
                    .target()
                    .strip_prefix(target)
                    .is_some_and(|suffix| suffix.starts_with("::"))
        })
}

fn application_file_filter(directive: &str) -> Result<EnvFilter, LogError> {
    EnvFilter::try_new(directive).map_err(|_| LogError::InvalidFilter)
}

fn oauth_recovery_file_filter() -> EnvFilter {
    format!("off,{OAUTH_RECOVERY_LOG_TARGET}=info")
        .parse()
        .expect("static OAuth recovery log filter is valid")
}

fn request_dump_file_filter() -> EnvFilter {
    format!("off,{REQUEST_DUMP_LOG_TARGET}=info")
        .parse()
        .expect("static request dump log filter is valid")
}

fn stdout_filter_directive(directive: &str, persistent_log_enabled: bool) -> String {
    if !persistent_log_enabled {
        directive.to_owned()
    } else {
        "off,gateway_startup=info,gateway_shutdown=info".to_owned()
    }
}

impl LogGuard {
    pub(crate) fn health_probe(&self) -> Arc<dyn HealthProbe> {
        self.health.clone()
    }
}

fn create_file_writer(
    config: &LoggingConfig,
    timezone: gateway_core::time::DeploymentTimeZone,
    retention_days: usize,
    prefix: &'static str,
    thread_name: &'static str,
    health: Arc<LogHealth>,
) -> Result<(LogSink, SinkGuard), LogError> {
    let maximum_bytes = config
        .file
        .max_file_size_mb
        .checked_mul(1024 * 1024)
        .ok_or(LogError::SizeOverflow)?;
    let writer = RotatingLogWriter::open(
        config.file.directory.clone(),
        prefix,
        maximum_bytes,
        retention_days,
        Arc::clone(&health),
        timezone,
    )?;
    Ok(LogSink::spawn(
        LogOutput::File(Box::new(writer)),
        thread_name,
        health,
    )?)
}

#[derive(Clone, Copy)]
struct DeploymentClock(gateway_core::time::DeploymentTimeZone);

impl tracing_subscriber::fmt::time::FormatTime for DeploymentClock {
    fn format_time(
        &self,
        writer: &mut tracing_subscriber::fmt::format::Writer<'_>,
    ) -> std::fmt::Result {
        write!(writer, "{}", self.0.local(chrono::Utc::now()).to_rfc3339())
    }
}

impl Drop for LogGuard {
    fn drop(&mut self) {
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        // 先通知全部输出，避免多个故障输出串行放大退出等待
        for output in &self.outputs {
            output.begin_shutdown();
        }
        for output in &mut self.outputs {
            output.finish_shutdown(deadline);
        }
    }
}
