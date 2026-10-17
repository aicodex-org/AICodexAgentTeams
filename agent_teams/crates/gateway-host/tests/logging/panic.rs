//! 验证普通线程 panic 的文件诊断与敏感 payload 隔离

use super::*;

#[test]
fn panic_records_location_without_exposing_payload() {
    const CHILD: &str = "logging::panic::panic_records_location_without_exposing_payload";
    const SECRET: &str = "synthetic-panic-secret-never-log";
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let directory = PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut config = logging_config(directory, false);
        config.logging.oauth_recovery = false;
        let bundle = runtime.block_on(gateway_host::initialize(config)).unwrap();
        assert!(std::thread::spawn(|| panic!("{SECRET}")).join().is_err());
        tracing::info!("process remains available after task panic");
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
    let body = read_log_file_set(directory.path(), APPLICATION_LOG_FILE_PREFIX);
    let panic = body
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["fields"]["event"] == "process.panic")
        .expect("panic event");
    assert_eq!(panic["level"], "ERROR");
    assert!(
        panic["fields"]["file"]
            .as_str()
            .unwrap()
            .ends_with("logging/panic.rs")
    );
    assert!(panic["fields"]["line"].as_u64().unwrap() > 0);
    assert!(body.contains("process remains available"));
    assert!(!body.contains(SECRET));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
}
