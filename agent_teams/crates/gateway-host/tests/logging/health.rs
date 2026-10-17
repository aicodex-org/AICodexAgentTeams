//! 验证只有敏感专用输出时，普通诊断缺口不会被报告为健康

use super::*;
use gateway_core::health::HealthState;

#[test]
fn dedicated_only_logging_reports_missing_application_diagnostics() {
    const CHILD: &str =
        "logging::health::dedicated_only_logging_reports_missing_application_diagnostics";
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let directory = PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut config = logging_config(directory, false);
        config.logging.stdout = false;
        config.logging.file.enabled = false;
        config.logging.oauth_recovery = true;
        let bundle = runtime.block_on(gateway_host::initialize(config)).unwrap();
        let HealthState::Degraded(reason) = runtime.block_on(bundle.logging_health_probe().check())
        else {
            panic!("dedicated logs do not cover application diagnostics");
        };
        assert!(reason.contains("ordinary logging disabled"));
        tracing::info!(target: OAUTH_RECOVERY_LOG_TARGET, "dedicated output remains available");
        drop(bundle);
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture"])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "info")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        read_log_file_set(directory.path(), OAUTH_RECOVERY_LOG_FILE_PREFIX)
            .contains("dedicated output remains available")
    );
    assert!(!log_file_set_exists(
        directory.path(),
        APPLICATION_LOG_FILE_PREFIX
    ));
}
