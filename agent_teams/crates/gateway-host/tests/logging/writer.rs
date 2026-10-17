//! 验证日志压缩、轮转与重启后按日期保留完整记录

use super::*;
use std::time::SystemTime;

const ROTATION_RECORDS: usize = 3;

#[test]
fn retention_preserves_complete_dates_across_compression_rotation_and_restart() {
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        with_file_logging(
            PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap()),
            true,
            || {
                let payload = "x".repeat(1024 * 1024);
                // 三条记录已跨过 1 MiB 轮转边界；保留日期的 25 个分段由下方单独构造
                for sequence in 0..ROTATION_RECORDS {
                    tracing::info!(target: REQUEST_DUMP_LOG_TARGET, sequence, payload, "retention record");
                    tracing::info!(target: APPLICATION_LOG_TARGET, sequence, payload, "retention record");
                    tracing::info!(target: OAUTH_RECOVERY_LOG_TARGET, sequence, payload, "retention record");
                }
            },
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let today = gateway_core::time::DeploymentTimeZone::default()
        .local(chrono::Utc::now())
        .date_naive();
    let mut retained = Vec::new();
    let mut expired = Vec::new();
    let mut temporary_archives = Vec::new();
    for (prefix, days) in [
        (APPLICATION_LOG_FILE_PREFIX, 7),
        (OAUTH_RECOVERY_LOG_FILE_PREFIX, 7),
        (REQUEST_DUMP_LOG_FILE_PREFIX, 1),
    ] {
        let boundary = today - chrono::Days::new(days);
        for segment in 1..=25 {
            let path = directory
                .path()
                .join(format!("{prefix}{boundary}.{segment}.log"));
            seed_log(&path, boundary, &format!("boundary-{segment}\n"));
            retained.push(path);
        }
        let old_date = boundary - chrono::Days::new(1);
        let path = directory.path().join(format!("{prefix}{old_date}.log"));
        seed_log(&path, old_date, "expired date\n");
        let temporary = path.with_extension("log.gz.tmp");
        fs::write(&temporary, "unfinished expired archive").unwrap();
        temporary_archives.push(temporary);
        expired.push(path);
        let orphan = directory
            .path()
            .join(format!("{prefix}{today}.99.log.gz.tmp"));
        fs::write(&orphan, "unfinished orphan archive").unwrap();
        temporary_archives.push(orphan);
        // 时区切换或恢复的文件按较近写入日期保护整个分段组
        let restored_date = old_date - chrono::Days::new(1);
        for segment in 0..=1 {
            let path = directory
                .path()
                .join(format!("{prefix}{restored_date}.{segment}.log"));
            seed_log(
                &path,
                if segment == 0 { today } else { restored_date },
                "restored date\n",
            );
            retained.push(path);
        }
    }
    let mut legacy_retained = Vec::new();
    for (prefix, days) in [
        ("codex-proxy-rs-application", 7),
        ("codex-proxy-rs-oauth-recovery", 7),
        ("codex-proxy-rs-request-dump", 1),
    ] {
        let boundary = today - chrono::Days::new(days);
        let old_date = boundary - chrono::Days::new(1);
        let orphan = directory
            .path()
            .join(format!("{prefix}.{old_date}.9.log.gz.tmp"));
        fs::write(&orphan, "unfinished legacy archive").unwrap();
        temporary_archives.push(orphan);
        for suffix in ["log", "2.log", "log.gz"] {
            let path = directory
                .path()
                .join(format!("{prefix}.{old_date}.{suffix}"));
            seed_log(&path, old_date, "expired legacy log\n");
            expired.push(path);
            let path = directory
                .path()
                .join(format!("{prefix}.{boundary}.{suffix}"));
            seed_log(&path, boundary, "retained legacy log\n");
            legacy_retained.push(path);
        }
    }
    let unrelated = directory.path().join("unmanaged.log");
    fs::write(&unrelated, "unmanaged\n").unwrap();
    let unrelated_temporary = directory.path().join("unmanaged.log.gz.tmp");
    fs::write(&unrelated_temporary, "unmanaged temporary\n").unwrap();
    let malformed_temporary = directory.path().join(format!(
        "{APPLICATION_LOG_FILE_PREFIX}invalid-date.log.gz.tmp"
    ));
    fs::write(&malformed_temporary, "malformed temporary\n").unwrap();
    let temporary_directory = directory.path().join(format!(
        "{APPLICATION_LOG_FILE_PREFIX}{today}.98.log.gz.tmp"
    ));
    fs::create_dir(&temporary_directory).unwrap();
    #[cfg(unix)]
    let temporary_link = directory.path().join(format!(
        "{APPLICATION_LOG_FILE_PREFIX}{today}.97.log.gz.tmp"
    ));
    #[cfg(unix)]
    std::os::unix::fs::symlink(&unrelated_temporary, &temporary_link).unwrap();
    // 归档发布前崩溃会留下原文件，也可能留下未完成的临时文件
    fs::write(
        retained[0].with_extension("log.gz.tmp"),
        "unfinished archive",
    )
    .unwrap();
    for run in 1..=2 {
        let output = Command::new(env::current_exe().unwrap())
            .args(["--exact", "logging::writer::retention_preserves_complete_dates_across_compression_rotation_and_restart"])
            .env(CHILD_PROCESS_ENV, "1").env(LOG_DIRECTORY_ENV, directory.path())
            .env("RUST_LOG", "off,logging_test_application=info").output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            retained
                .iter()
                .all(|path| path.with_extension("log.gz").exists())
        );
        assert!(
            expired
                .iter()
                .all(|path| !path.exists() && !path.with_extension("log.gz").exists())
        );
        assert!(unrelated.exists());
        assert!(unrelated_temporary.exists() && malformed_temporary.exists());
        assert!(temporary_directory.is_dir());
        #[cfg(unix)]
        assert!(temporary_link.is_symlink());
        assert!(temporary_archives.iter().all(|path| !path.exists()));
        assert!(legacy_retained.iter().all(|path| path.exists()));

        for prefix in [
            APPLICATION_LOG_FILE_PREFIX,
            OAUTH_RECOVERY_LOG_FILE_PREFIX,
            REQUEST_DUMP_LOG_FILE_PREFIX,
        ] {
            let body = read_log_file_set(directory.path(), prefix);
            let mut sequences = [0; ROTATION_RECORDS];
            let mut boundaries = std::collections::BTreeSet::new();
            for line in body.lines() {
                if let Some(segment) = line.strip_prefix("boundary-") {
                    boundaries.insert(segment.parse::<usize>().unwrap());
                } else if line.starts_with('{') {
                    #[derive(serde::Deserialize)]
                    struct Record {
                        fields: Fields,
                    }
                    #[derive(serde::Deserialize)]
                    struct Fields {
                        sequence: Option<usize>,
                    }
                    if let Some(sequence) = serde_json::from_str::<Record>(line)
                        .unwrap()
                        .fields
                        .sequence
                    {
                        sequences[sequence] += 1;
                    }
                }
            }
            assert_eq!(sequences, [run; ROTATION_RECORDS]);
            assert_eq!(
                boundaries,
                (1..=25).collect(),
                "whole boundary date must survive"
            );
            let today_segments = fs::read_dir(directory.path())
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&format!("{prefix}{today}"))
                })
                .count();
            assert!(today_segments >= 2, "records must exercise size rotation");
        }
    }
}

#[test]
fn archive_publish_failure_preserves_source_cleans_temporary_and_recovers_on_restart() {
    use gateway_core::health::HealthState;

    const EXPECT_FAILURE_ENV: &str = "AICODEX_AGENT_TEAMS_LOGGING_TEST_ARCHIVE_FAILURE";
    if env::var_os(CHILD_PROCESS_ENV).is_some() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let bundle = runtime
            .block_on(gateway_host::initialize(logging_config(
                PathBuf::from(env::var_os(LOG_DIRECTORY_ENV).unwrap()),
                false,
            )))
            .unwrap();
        let health = runtime.block_on(bundle.logging_health_probe().check());
        if env::var_os(EXPECT_FAILURE_ENV).is_some() {
            assert!(matches!(health, HealthState::Degraded(_)));
        } else {
            assert_eq!(health, HealthState::Healthy);
        }
        tracing::info!(target: APPLICATION_LOG_TARGET, "record after archive maintenance");
        drop(bundle);
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let date = gateway_core::time::DeploymentTimeZone::default()
        .local(chrono::Utc::now())
        .date_naive()
        - chrono::Days::new(1);
    let source = directory
        .path()
        .join(format!("{APPLICATION_LOG_FILE_PREFIX}{date}.3.log"));
    seed_log(&source, date, "record before archive maintenance\n");
    let archive = source.with_extension("log.gz");
    let temporary = source.with_extension("log.gz.tmp");
    // 用目录阻止归档发布，覆盖临时文件已经完整写入后的失败出口
    fs::create_dir(&archive).unwrap();
    for failure in [true, false] {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args(["--exact", "logging::writer::archive_publish_failure_preserves_source_cleans_temporary_and_recovers_on_restart"])
            .env(CHILD_PROCESS_ENV, "1")
            .env(LOG_DIRECTORY_ENV, directory.path())
            .env("RUST_LOG", "off,logging_test_application=info");
        if failure {
            command.env(EXPECT_FAILURE_ENV, "1");
        } else {
            command.env_remove(EXPECT_FAILURE_ENV);
            fs::remove_dir(&archive).unwrap();
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!temporary.exists());
        if failure {
            assert_eq!(
                fs::read_to_string(&source).unwrap(),
                "record before archive maintenance\n"
            );
        } else {
            assert!(!source.exists());
            let mut body = String::new();
            flate2::read::GzDecoder::new(fs::File::open(&archive).unwrap())
                .read_to_string(&mut body)
                .unwrap();
            assert_eq!(body, "record before archive maintenance\n");
            assert!(
                read_log_file_set(directory.path(), APPLICATION_LOG_FILE_PREFIX)
                    .contains("record after archive maintenance")
            );
        }
    }
}

fn seed_log(path: &Path, date: chrono::NaiveDate, body: &str) {
    fs::write(path, body).unwrap();
    let modified = gateway_core::time::DeploymentTimeZone::default()
        .date_start(date)
        .unwrap();
    fs::File::open(path)
        .unwrap()
        .set_modified(SystemTime::from(modified))
        .unwrap();
}
