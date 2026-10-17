//! 按输出与操作归因日志完整性缺口，健康检查不依赖故障中的日志队列

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use gateway_core::health::{HealthProbe, HealthState};

pub(super) struct LogHealth {
    name: &'static str,
    write_failures: AtomicU64,
    maintenance_failures: AtomicU64,
    rejected_records: AtomicU64,
    rejected_errors: AtomicU64,
    reported: AtomicBool,
    last_failure: Mutex<Option<String>>,
}

impl LogHealth {
    pub(super) fn new(name: &'static str) -> Arc<Self> {
        Arc::new(Self {
            name,
            write_failures: AtomicU64::new(0),
            maintenance_failures: AtomicU64::new(0),
            rejected_records: AtomicU64::new(0),
            rejected_errors: AtomicU64::new(0),
            reported: AtomicBool::new(false),
            last_failure: Mutex::new(None),
        })
    }

    pub(super) fn write_failed(&self, operation: &'static str, kind: io::ErrorKind) {
        self.write_failures.fetch_add(1, Ordering::Relaxed);
        self.failed("log completeness compromised", operation, kind);
    }

    pub(super) fn maintenance_failed(&self, operation: &'static str, kind: io::ErrorKind) {
        self.maintenance_failures.fetch_add(1, Ordering::Relaxed);
        self.failed("archive/retention maintenance failed", operation, kind);
    }

    fn failed(&self, summary: &'static str, operation: &'static str, kind: io::ErrorKind) {
        let failure = format!(
            "sink={} operation={operation} kind={kind:?} at={}",
            self.name,
            chrono::Utc::now().to_rfc3339(),
        );
        *self
            .last_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(failure.clone());
        if !self.reported.swap(true, Ordering::Relaxed) {
            // stderr 也可能阻塞；每个输出至多一个旁路线程，关闭路径不等待它
            // 原始 I/O 错误、路径和日志正文不进入兜底消息
            let _ = std::thread::Builder::new()
                .name("gateway-log-fallback".to_owned())
                .spawn(move || eprintln!("logging: {summary}; {failure}"));
        }
    }

    pub(super) fn rejected(&self, important: bool) {
        self.rejected_records.fetch_add(1, Ordering::Relaxed);
        if important {
            self.rejected_errors.fetch_add(1, Ordering::Relaxed);
        }
        // 接收路径只更新计数，不等待输出、不递归记录拥堵日志
    }

    fn state(&self) -> HealthState {
        let failures = self.write_failures.load(Ordering::Relaxed);
        let rejected = self.rejected_records.load(Ordering::Relaxed);
        let errors = self.rejected_errors.load(Ordering::Relaxed);
        let maintenance = self.maintenance_failures.load(Ordering::Relaxed);
        if failures == 0 && rejected == 0 && maintenance == 0 {
            return HealthState::Healthy;
        }
        let last = self
            .last_failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let message = format!(
            "sink={}: {failures} output failures, {rejected} rejected records ({errors} warnings/errors), {maintenance} maintenance failures since startup; {}",
            self.name,
            last.as_deref().unwrap_or("operation=enqueue"),
        );
        if failures > 0 || rejected > 0 {
            HealthState::Unhealthy(message)
        } else {
            HealthState::Degraded(message)
        }
    }
}

pub(super) struct LoggingHealth(pub(super) Vec<Arc<LogHealth>>);

impl HealthProbe for LoggingHealth {
    fn name(&self) -> &'static str {
        // 保留探针身份，避免已有运维规则因增加 stdout 覆盖而失效
        "file_logging"
    }

    fn check(&self) -> BoxFuture<'_, HealthState> {
        Box::pin(async move {
            let mut unhealthy = false;
            let mut failures = Vec::new();
            if !self
                .0
                .iter()
                .any(|output| matches!(output.name, "stdout" | "application"))
            {
                failures.push(
                    "ordinary logging disabled: stdout and application sinks are disabled"
                        .to_owned(),
                );
            }
            for output in &self.0 {
                match output.state() {
                    HealthState::Healthy => {}
                    HealthState::Unhealthy(message) => {
                        unhealthy = true;
                        failures.push(message);
                    }
                    HealthState::Degraded(message) => failures.push(message),
                }
            }
            if failures.is_empty() {
                HealthState::Healthy
            } else if unhealthy {
                HealthState::Unhealthy(format!(
                    "log completeness compromised: {}",
                    failures.join("; ")
                ))
            } else {
                HealthState::Degraded(failures.join("; "))
            }
        })
    }
}
