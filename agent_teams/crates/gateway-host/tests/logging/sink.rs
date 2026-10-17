//! 验证文件日志队列排空与写入失败后的完整性状态

use super::*;
use gateway_core::health::HealthState;
use std::time::Duration;

#[test]
fn concurrent_shutdown_accounts_for_every_submitted_record() {
    const CHILD: &str = "logging::sink::concurrent_shutdown_accounts_for_every_submitted_record";
    const PRODUCERS: usize = 8;
    const BEFORE: usize = 100;
    const RACING: usize = 1_000;
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let directory = PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut config = logging_config(directory.clone(), false);
        config.logging.oauth_recovery = false;
        config.logging.file.max_file_size_mb = 64;
        let bundle = runtime.block_on(gateway_host::initialize(config)).unwrap();
        let health = bundle.logging_health_probe();
        let start_shutdown = std::sync::Arc::new(std::sync::Barrier::new(PRODUCERS + 1));
        let shutdown_complete = std::sync::Arc::new(std::sync::Barrier::new(PRODUCERS + 1));
        let producers = (0..PRODUCERS)
            .map(|producer| {
                let start_shutdown = std::sync::Arc::clone(&start_shutdown);
                let shutdown_complete = std::sync::Arc::clone(&shutdown_complete);
                std::thread::spawn(move || {
                    for sequence in 0..BEFORE {
                        tracing::info!(target: APPLICATION_LOG_TARGET, producer, sequence,
                            marker="concurrent-shutdown-record");
                    }
                    start_shutdown.wait();
                    for sequence in BEFORE..BEFORE + RACING {
                        tracing::info!(target: APPLICATION_LOG_TARGET, producer, sequence,
                            marker="concurrent-shutdown-record");
                    }
                    shutdown_complete.wait();
                    // 关闭后仍持有全局 subscriber，拒收也必须计入完整性状态
                    tracing::info!(target: APPLICATION_LOG_TARGET, producer, sequence=BEFORE + RACING,
                        marker="concurrent-shutdown-record");
                })
            })
            .collect::<Vec<_>>();
        start_shutdown.wait();
        drop(bundle);
        shutdown_complete.wait();
        for producer in producers {
            producer.join().unwrap();
        }
        let HealthState::Unhealthy(message) = runtime.block_on(health.check()) else {
            panic!("records submitted after shutdown must be reported as rejected");
        };
        let rejected = message
            .split_once("sink=application: 0 output failures, ")
            .unwrap_or_else(|| panic!("unexpected logging failure: {message}"))
            .1
            .split_once(" rejected records")
            .unwrap()
            .0
            .parse::<usize>()
            .unwrap();
        assert!(rejected >= PRODUCERS, "{message}");
        let body = read_log_file_set(&directory, APPLICATION_LOG_FILE_PREFIX);
        let records = body
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .filter(|record| record["fields"]["marker"] == "concurrent-shutdown-record")
            .collect::<Vec<_>>();
        let identities = records
            .iter()
            .map(|record| {
                (
                    record["fields"]["producer"].as_u64().unwrap(),
                    record["fields"]["sequence"].as_u64().unwrap(),
                )
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            identities.len(),
            records.len(),
            "duplicate persisted records"
        );
        assert_eq!(
            records.len() + rejected,
            PRODUCERS * (BEFORE + RACING + 1),
            "every submission must be persisted or counted as rejected: {message}"
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture"])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "off,logging_test_application=info")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn permanently_blocked_output_stops_waiting_at_shutdown_budget() {
    const CHILD: &str =
        "logging::sink::permanently_blocked_output_stops_waiting_at_shutdown_budget";
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let directory = PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut config = logging_config(directory.clone(), false);
        config.logging.oauth_recovery = false;
        let bundle = runtime.block_on(gateway_host::initialize(config)).unwrap();
        let health = bundle.logging_health_probe();
        let date = gateway_core::time::DeploymentTimeZone::default()
            .local(chrono::Utc::now())
            .date_naive();
        let fifo = directory.join(format!("{APPLICATION_LOG_FILE_PREFIX}{date}.1.log"));
        assert!(Command::new("mkfifo").arg(fifo).status().unwrap().success());
        let payload = "x".repeat(1024 * 1024);
        tracing::info!(target: APPLICATION_LOG_TARGET, payload, "fill first segment");
        tracing::info!(target: APPLICATION_LOG_TARGET, "block next segment without a reader");
        let initial = directory.join(format!("{APPLICATION_LOG_FILE_PREFIX}{date}.log"));
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                while fs::metadata(&initial).unwrap().len() < 1024 * 1024 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
        });
        // 普通与预留额度都耗尽时，拒收的高等级记录必须可观测
        for sequence in 0..5_000 {
            tracing::info!(target: APPLICATION_LOG_TARGET, sequence, "blocked regular output");
        }
        for sequence in 0..600 {
            tracing::warn!(target: APPLICATION_LOG_TARGET, sequence, "blocked reserved output");
        }
        let HealthState::Unhealthy(message) = runtime.block_on(health.check()) else {
            panic!("queue exhaustion must remain visible");
        };
        assert!(message.contains("sink=application"), "{message}");
        assert!(!message.contains("(0 warnings/errors)"), "{message}");
        let started = std::time::Instant::now();
        drop(bundle);
        assert!(started.elapsed() < Duration::from_secs(7));
        let HealthState::Unhealthy(message) = runtime.block_on(health.check()) else {
            panic!("shutdown timeout must compromise log completeness");
        };
        assert!(message.contains("sink=application"), "{message}");
        assert!(message.contains("operation=shutdown"), "{message}");
        assert!(message.contains("TimedOut"), "{message}");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture"])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "off,logging_test_application=info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // 旧实现会永远卡住；父进程负责终止，避免回归拖死整个测试进程
    let deadline = std::time::Instant::now() + Duration::from_secs(12);
    loop {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "log shutdown exceeded its budget: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn closed_stdout_pipe_reports_output_failure() {
    const CHILD: &str = "logging::sink::closed_stdout_pipe_reports_output_failure";
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let directory = PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap());
        fs::write(directory.join("ready"), b"").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !directory.join("released").exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "parent did not close stdout"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut config = logging_config(PathBuf::new(), false);
        config.logging.stdout = true;
        config.logging.file.enabled = false;
        config.logging.oauth_recovery = false;
        let bundle = runtime.block_on(gateway_host::initialize(config)).unwrap();
        let health = bundle.logging_health_probe();
        tracing::error!(target: APPLICATION_LOG_TARGET, "stdout reader has closed");
        drop(bundle);
        let HealthState::Unhealthy(message) = runtime.block_on(health.check()) else {
            panic!("stdout write errors must remain visible");
        };
        assert!(message.contains("sink=stdout"), "{message}");
        assert!(message.contains("BrokenPipe"), "{message}");
        // libtest 自身也写 stdout；验证完毕后直接退出，避免其汇总写入故障管道
        std::process::exit(0);
    }
    let directory = tempfile::tempdir().unwrap();
    let (reader, writer) = std::io::pipe().unwrap();
    let mut child = Command::new(env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture", "--quiet"])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "off,logging_test_application=info")
        .stdout(writer)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // 先让 libtest 输出启动信息，再关闭管道，故障仅由被测日志触发
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !directory.path().join("ready").exists() {
        if std::time::Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "stdout child did not start: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(reader);
    fs::write(directory.path().join("released"), b"").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn file_queue_drains_every_record_before_normal_shutdown() {
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        with_file_logging(
            PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap()),
            true,
            || {
                let payload = "x".repeat(16 * 1024);
                for sequence in 0..2000 {
                    tracing::info!(target: REQUEST_DUMP_LOG_TARGET, sequence, payload, "queue record");
                }
            },
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "logging::sink::file_queue_drains_every_record_before_normal_shutdown",
        ])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "off")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body = read_log_file_set(directory.path(), REQUEST_DUMP_LOG_FILE_PREFIX);
    let mut sequences = body
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["fields"]["sequence"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    sequences.sort_unstable();
    assert_eq!(sequences, (0..2000).collect::<Vec<_>>());
}

#[test]
fn write_failure_marks_log_completeness_unhealthy_even_after_writes_recover() {
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let directory = PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let bundle = runtime
            .block_on(gateway_host::initialize(logging_config(
                directory.clone(),
                true,
            )))
            .unwrap();
        let health = bundle.logging_health_probe();
        assert_eq!(runtime.block_on(health.check()), HealthState::Healthy);
        let blocked = directory.join(format!(
            "{REQUEST_DUMP_LOG_FILE_PREFIX}{}.1.log",
            gateway_core::time::DeploymentTimeZone::default()
                .local(chrono::Utc::now())
                .date_naive()
        ));
        fs::create_dir(&blocked).unwrap();
        let payload = "x".repeat(1024 * 1024);
        tracing::info!(target: REQUEST_DUMP_LOG_TARGET, payload, marker="before-failure");
        tracing::info!(target: REQUEST_DUMP_LOG_TARGET, payload, marker="cannot-write");
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if matches!(health.check().await, HealthState::Unhealthy(_)) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        });
        fs::remove_dir(&blocked).unwrap();
        tracing::info!(target: REQUEST_DUMP_LOG_TARGET, marker="after-recovery");
        drop(bundle);
        assert!(matches!(
            runtime.block_on(health.check()),
            HealthState::Unhealthy(_)
        ));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args(["--nocapture", "--exact", "logging::sink::write_failure_marks_log_completeness_unhealthy_even_after_writes_recover"])
        .env(CHILD_PROCESS_ENV, "1").env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "off").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("log completeness compromised"));
    let body = read_log_file_set(directory.path(), REQUEST_DUMP_LOG_FILE_PREFIX);
    assert!(body.contains("before-failure") && body.contains("after-recovery"));
    assert!(!body.contains("cannot-write"));
}

#[cfg(unix)]
#[test]
fn blocked_file_output_keeps_http_and_streaming_responsive_and_reserves_errors() {
    const CHILD: &str = "logging::sink::blocked_file_output_keeps_http_and_streaming_responsive_and_reserves_errors";
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let directory = PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut config = logging_config(directory.clone(), false);
        config.logging.oauth_recovery = false;
        config.logging.file.max_file_size_mb = 8;
        let bundle = runtime.block_on(gateway_host::initialize(config)).unwrap();
        let health = bundle.logging_health_probe();
        let date = gateway_core::time::DeploymentTimeZone::default()
            .local(chrono::Utc::now())
            .date_naive();
        let fifo = directory.join(format!("{APPLICATION_LOG_FILE_PREFIX}{date}.1.log"));
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let (release, released) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            // 超时只用于让旧的同步实现能退出测试，接口必须在解除阻塞之前完成
            let _ = released.recv_timeout(Duration::from_secs(3));
            let mut body = String::new();
            fs::File::open(fifo)
                .unwrap()
                .read_to_string(&mut body)
                .unwrap();
            body
        });
        let payload = "x".repeat(8 * 1024 * 1024);
        tracing::info!(target: APPLICATION_LOG_TARGET, payload, "rotate before requests");
        tracing::info!(target: APPLICATION_LOG_TARGET, "block on opening next segment");
        runtime.block_on(async {
            let initial = directory.join(format!("{APPLICATION_LOG_FILE_PREFIX}{date}.log"));
            tokio::time::timeout(Duration::from_secs(2), async {
                while fs::metadata(&initial).unwrap().len() < 8 * 1024 * 1024 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }).await.unwrap();
            use futures::{SinkExt as _, StreamExt as _};
            let app = axum::Router::new()
                .route("/v1/log-test", axum::routing::get(|| async {
                    tracing::error!(target: APPLICATION_LOG_TARGET, path="v1", "critical_after_congestion");
                    "ok"
                }))
                .route("/admin/log-test", axum::routing::get(|| async {
                    tracing::error!(target: APPLICATION_LOG_TARGET, path="admin", "critical_after_congestion");
                    "ok"
                }))
                .route("/v1/stream-test", axum::routing::get(|| async {
                    tracing::error!(target: APPLICATION_LOG_TARGET, path="stream", "critical_after_congestion");
                    axum::response::Sse::new(futures::stream::iter([
                        Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data("first")),
                        Ok(axum::response::sse::Event::default().data("last")),
                    ]))
                }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
            let started = std::time::Instant::now();
            for sequence in 0..5_000 {
                tracing::info!(target: APPLICATION_LOG_TARGET, sequence, "congested regular log");
            }
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            for path in ["/v1/log-test", "/admin/log-test", "/v1/stream-test"] {
                let response = client.get(format!("http://{address}{path}")).send().await.unwrap();
                assert!(response.status().is_success());
                let body = response.text().await.unwrap();
                if path.ends_with("stream-test") {
                    assert!(body.contains("first") && body.contains("last"));
                } else {
                    assert_eq!(body, "ok");
                }
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let websocket_server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                tracing::error!(target: APPLICATION_LOG_TARGET, path="websocket", "critical_after_congestion");
                socket.send(tokio_tungstenite::tungstenite::Message::Text("frame".into())).await.unwrap();
            });
            let stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let (mut websocket, _) = tokio_tungstenite::client_async(format!("ws://{address}/"), stream).await.unwrap();
            assert_eq!(websocket.next().await.unwrap().unwrap().into_text().unwrap(), "frame");
            websocket_server.await.unwrap();
            let elapsed = started.elapsed();
            assert!(elapsed < Duration::from_secs(1), "interfaces waited for logging: {elapsed:?}");
            eprintln!("paused log output: 5000 submissions and HTTP/admin/SSE/WS completed in {elapsed:?}");
            let HealthState::Unhealthy(message) = health.check().await else { panic!("queue overflow must be visible") };
            assert!(message.contains("0 warnings/errors"), "{message}");
            server.abort();
        });
        release.send(()).unwrap();
        drop(bundle);
        let body = reader.join().unwrap();
        assert_eq!(
            body.lines()
                .filter(|line| line.contains("critical_after_congestion"))
                .count(),
            4
        );
        for line in body.lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", CHILD, "--nocapture"])
        .env(CHILD_PROCESS_ENV, "1")
        .env(LOG_DIRECTORY_ENV, directory.path())
        .env("RUST_LOG", "off,logging_test_application=info")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{}", String::from_utf8_lossy(&output.stderr));
}
