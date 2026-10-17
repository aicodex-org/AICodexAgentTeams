//! 验证服务组装为 HTTPS 代理探测传递自定义 CA

use std::{fs, process::Command};

#[tokio::test]
async fn proxy_probe_should_use_provider_custom_ca_for_https_proxies() {
    use gateway_admin::ports::proxy::ProxyProbe;
    use gateway_core::account::OutboundProxy;
    use gateway_host::proxy_probe::HttpProxyProbe;
    use std::{sync::Arc, time::Duration};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            ServerConfig,
            pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _},
        },
    };

    const CHILD_ENV: &str = "AICODEX_AGENT_TEAMS_TEST_PROXY_TLS_DIRECTORY";
    let Ok(directory) = std::env::var(CHILD_ENV) else {
        let directory = tempfile::tempdir().unwrap();
        generate_proxy_test_certificates(directory.path());
        // 使用子进程隔离环境变量，避免并行测试读取到临时 CA 配置
        for ca_env in ["CODEX_CA_CERTIFICATE", "SSL_CERT_FILE"] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "bootstrap::server::proxy_probe_should_use_provider_custom_ca_for_https_proxies",
                    "--nocapture",
                ])
                .env(CHILD_ENV, directory.path())
                .env_remove("CODEX_CA_CERTIFICATE")
                .env_remove("SSL_CERT_FILE")
                .env(ca_env, directory.path().join("ca.pem"));
            if ca_env == "CODEX_CA_CERTIFICATE" {
                child.env(
                    "SSL_CERT_FILE",
                    directory.path().join("missing-fallback.pem"),
                );
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{ca_env}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return;
    };
    provider_openai::ensure_rustls_provider();
    let directory = std::path::Path::new(&directory);
    let certificate = CertificateDer::from_pem_file(directory.join("server.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_file(directory.join("server.key")).unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(
        ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key)
            .unwrap(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy =
        OutboundProxy::parse(&format!("https://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut stream = acceptor.accept(socket).await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert!(request.len() < 8192);
            request.push(stream.read_u8().await.unwrap());
        }
        assert!(request.starts_with(b"GET http://unresolvable.invalid/ip "));
        let body = "{\"ip\":\"203.0.113.8\"}";
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    });
    let probe = HttpProxyProbe::new("http://unresolvable.invalid/ip")
        .with_client_builder(provider_openai::build_reqwest_client_with_custom_ca);
    let result = probe.test(&proxy, false).await;
    assert!(result.success, "{}", result.message);
    assert_eq!(result.exit_ip.unwrap().to_string(), "203.0.113.8");
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

fn generate_proxy_test_certificates(directory: &std::path::Path) {
    let openssl = |args: &[&str]| {
        let output = Command::new("openssl")
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    openssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "ca.key",
        "-out",
        "ca.pem",
        "-days",
        "1",
        "-subj",
        "/CN=Proxy Test CA",
    ]);
    openssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-nodes",
        "-keyout",
        "server.key",
        "-out",
        "server.csr",
        "-subj",
        "/CN=localhost",
    ]);
    fs::write(directory.join("extensions"), "subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n").unwrap();
    openssl(&[
        "x509",
        "-req",
        "-in",
        "server.csr",
        "-CA",
        "ca.pem",
        "-CAkey",
        "ca.key",
        "-CAcreateserial",
        "-out",
        "server.pem",
        "-days",
        "1",
        "-extfile",
        "extensions",
    ]);
}

#[test]
fn startup_failures_are_recorded_before_server_and_cli_log_guards_close() {
    let example = include_str!("../../../../../deploy/config.example.yaml");
    let source = config::Config::builder()
        .add_source(config::File::from_str(example, config::FileFormat::Yaml))
        .build()
        .unwrap();
    let template: serde_json::Value = source.try_deserialize().unwrap();
    for arguments in [
        vec!["serve"],
        vec!["plugin", "--help"],
        vec!["plugin", "missing", "run"],
    ] {
        let directory = tempfile::tempdir().unwrap();
        let deploy = directory.path().join("deploy");
        fs::create_dir(&deploy).unwrap();
        let mut document = template.clone();
        document["store"]["database"]["password"] = serde_json::json!("1".repeat(48));
        document["store"]["redis"]["password"] = serde_json::json!("2".repeat(48));
        // URL 合法但 PostgreSQL 选项无效，在日志初始化后立即失败，不访问外部服务
        document["store"]["database"]["url"] =
            serde_json::json!("postgres://test@127.0.0.1/test?sslmode=invalid-test-mode");
        document["admin"]["default_password"] = serde_json::json!("startup-test-secret-password");
        document["host"]["logging"]["stdout"] = serde_json::json!(true);
        document["host"]["logging"]["file"]["enabled"] = serde_json::json!(arguments[0] == "serve");
        document["host"]["logging"]["file"]["directory"] = serde_json::json!("logs");
        fs::write(
            deploy.join("config.yaml"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_AICodexAgentTeams"))
            .args(&arguments)
            .current_dir(directory.path())
            .env_clear()
            .output()
            .unwrap();
        assert!(!output.status.success());
        let logs: String = fs::read_dir(deploy.join("logs"))
            .unwrap_or_else(|error| {
                panic!(
                    "{arguments:?}: {error}; stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                )
            })
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        assert!(
            logs.contains("服务初始化失败，启动已终止"),
            "{arguments:?}: {logs}"
        );
        assert!(logs.contains("\"stage\":\"store\""));
        assert!(logs.contains("\"category\":\"postgres_unavailable\""));
        assert!(!logs.contains("startup-test-secret-password"));
        assert!(!logs.contains(&"1".repeat(48)));
        if arguments[0] == "plugin" {
            assert!(output.stdout.is_empty());
        }
    }
}
