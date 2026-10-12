//! 官方加密存储、迁移、损坏保护和 OS 文件权限用例
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use super::support::*;
use crate::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

#[tokio::test]
async fn encryption_permissions_and_host_identity() {
    let f = Fixture::new(offline());
    let state = saved_state();
    f.write(&state).await;
    let raw = String::from_utf8(f.raw()).unwrap();
    for secret in [
        "previous-test-access",
        "previous-test-refresh",
        "private-id-token",
        "private@example.invalid",
        "private-subject",
        "registered_test_client",
    ] {
        assert!(!raw.contains(secret));
    }
    let envelope: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(envelope["version"], 3);
    assert_eq!(envelope["provider"], "test-aes-gcm");
    assert_eq!(envelope.as_object().unwrap().len(), 3);
    assert_eq!(f.read().await, state);
    let lock = f.store.lock(&CancellationToken::new()).await.unwrap();
    let id = lock.host_id().await.unwrap();
    assert_eq!(lock.host_id().await.unwrap(), id);
    drop(lock);
    assert_eq!(
        serde_json::from_slice::<Value>(
            &std::fs::read(f.directory.path().join("chatgpt-host.json")).unwrap()
        )
        .unwrap(),
        json!({"version":1,"id":id})
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(f.directory.path()).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(f.directory.path().join("chatgpt-auth.json"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }
}
#[tokio::test]
async fn plaintext_v1_v2_migrate_without_backup_or_temp() {
    for version in [1, 2] {
        let f = Fixture::new(offline());
        let state = saved_state();
        let legacy = if version == 2 {
            state.clone()
        } else {
            let mut p = state["profiles"][0].clone();
            p.as_object_mut().unwrap().remove("id");
            p.as_object_mut().unwrap().remove("label");
            p
        };
        f.write_raw(&serde_json::to_vec(&legacy).unwrap());
        let migrated = f.read().await;
        assert_eq!(
            serde_json::from_slice::<Value>(&f.raw()).unwrap()["version"],
            3
        );
        assert_eq!(
            migrated["profiles"][0]["credentials"]["refreshToken"],
            "previous-test-refresh"
        );
        if version == 1 {
            assert_eq!(migrated["profiles"][0]["requiresNewRegistration"], true);
            assert_eq!(
                f.client
                    .sign_in(SignInOptions::default())
                    .await
                    .unwrap_err()
                    .code,
                "registration_upgrade_required"
            );
        } else {
            assert_eq!(migrated, state);
        }
        assert_eq!(f.read().await, migrated);
        assert_eq!(std::fs::read_dir(f.directory.path()).unwrap().count(), 1);
    }
}
#[test]
fn encryption_and_configuration_are_validated_at_construction() {
    let dir = tempfile::tempdir().unwrap();
    let mut provider = Encryption::new();
    provider.id = "../bad".into();
    assert_eq!(
        ChatGPT::with_transport(
            Fixture::config(&dir, std::sync::Arc::new(provider)),
            offline()
        )
        .err()
        .unwrap()
        .code,
        "invalid_config"
    );
    for (name, id, path) in [
        ("", "valid", None),
        ("valid", "INVALID", None),
        ("valid", "valid", Some(std::path::PathBuf::from("relative"))),
    ] {
        let mut config = Fixture::config(&dir, std::sync::Arc::new(Encryption::new()));
        config.app_name = name.into();
        config.app_id = id.into();
        config.storage_dir = path;
        assert_eq!(
            ChatGPT::with_transport(config, offline())
                .err()
                .unwrap()
                .code,
            "invalid_config"
        );
    }
}
#[tokio::test]
async fn unavailable_encryption_blocks_oauth_and_files() {
    let mock = offline();
    let f = Fixture::new(mock.clone());
    f.provider.available.store(false, Ordering::SeqCst);
    assert_eq!(
        f.client
            .sign_in(SignInOptions::default())
            .await
            .unwrap_err()
            .code,
        "storage_encryption_unavailable"
    );
    let lock = f.store.lock(&CancellationToken::new()).await.unwrap();
    assert_eq!(
        lock.host_id().await.unwrap_err().code,
        "storage_encryption_unavailable"
    );
    assert_eq!(
        lock.write(&saved_state()).await.unwrap_err().code,
        "storage_encryption_unavailable"
    );
    drop(lock);
    assert!(mock.calls.lock().unwrap().is_empty());
    assert_eq!(std::fs::read_dir(f.directory.path()).unwrap().count(), 0);
}
#[tokio::test]
async fn failed_migration_and_writes_preserve_original_bytes() {
    for legacy in [false, true] {
        let f = Fixture::new(offline());
        if legacy {
            f.write_raw(&serde_json::to_vec(&saved_state()).unwrap());
        } else {
            f.write(&saved_state()).await;
        }
        let before = f.raw();
        f.provider.fail_encrypt.store(true, Ordering::SeqCst);
        let error = if legacy {
            f.client
                .sign_in(SignInOptions {
                    new_profile: true,
                    ..Default::default()
                })
                .await
                .unwrap_err()
        } else {
            f.store
                .lock(&CancellationToken::new())
                .await
                .unwrap()
                .write(&saved_state())
                .await
                .unwrap_err()
        };
        assert_eq!(error.code, "storage_encryption_failed");
        assert!(!error.message.contains("private-provider-error"));
        assert_eq!(f.raw(), before);
        assert_eq!(std::fs::read_dir(f.directory.path()).unwrap().count(), 1);
    }
}
#[tokio::test]
async fn oversized_envelope_does_not_replace_plaintext() {
    let f = Fixture::new(offline());
    let mut state = saved_state();
    state["profiles"][0]["credentials"]["accessToken"] = "x".repeat(1_600_000).into();
    let original = serde_json::to_vec(&state).unwrap();
    f.write_raw(&original);
    assert_eq!(
        f.store
            .lock(&CancellationToken::new())
            .await
            .unwrap()
            .read()
            .await
            .unwrap_err()
            .code,
        "storage_too_large"
    );
    assert_eq!(f.raw(), original);
    assert_eq!(std::fs::read_dir(f.directory.path()).unwrap().count(), 1);
}
#[tokio::test]
async fn unreadable_storage_is_preserved_across_every_mutation() {
    for failure in [
        "unavailable",
        "decrypt",
        "provider",
        "envelope",
        "base64",
        "json",
        "decrypted-state",
    ] {
        let mock = offline();
        let f = Fixture::new(mock.clone());
        f.write(&saved_state()).await;
        let mut envelope: Value = serde_json::from_slice(&f.raw()).unwrap();
        let expected = match failure {
            "unavailable" => {
                f.provider.available.store(false, Ordering::SeqCst);
                "storage_encryption_unavailable"
            }
            "decrypt" => {
                f.provider.fail_decrypt.store(true, Ordering::SeqCst);
                "storage_decryption_failed"
            }
            "provider" => {
                envelope["provider"] = "other-provider".into();
                "storage_provider_mismatch"
            }
            "envelope" => {
                envelope["version"] = 99.into();
                "storage_encrypted_invalid"
            }
            "base64" => {
                envelope["ciphertext"] = "not base64!".into();
                "storage_encrypted_invalid"
            }
            "decrypted-state" => {
                envelope["ciphertext"] = STANDARD
                    .encode(
                        f.provider
                            .encrypt("{\"version\":2,\"profiles\":\"invalid\"}")
                            .await
                            .unwrap(),
                    )
                    .into();
                "storage_decryption_failed"
            }
            _ => "storage_invalid",
        };
        let raw = if failure == "json" {
            b"{\"version\":3,\"ciphertext\":".to_vec()
        } else {
            serde_json::to_vec(&envelope).unwrap()
        };
        f.write_raw(&raw);
        let e = f
            .client
            .sign_in(SignInOptions {
                new_profile: true,
                ..Default::default()
            })
            .await
            .unwrap_err();
        assert_eq!(e.code, expected, "{failure}");
        assert_eq!(f.raw(), raw);
        assert_eq!(
            f.client.select_profile("profile-1").await.unwrap_err().code,
            expected
        );
        assert_eq!(f.raw(), raw);
        assert_eq!(f.client.disconnect().await.unwrap_err().code, expected);
        assert_eq!(f.raw(), raw);
        let session = f.client.get_session().await;
        assert_eq!(session.error.unwrap().code, expected);
        assert!(
            !serde_json::to_string(&f.client.list_profiles().await.err().unwrap())
                .unwrap()
                .contains("private-provider-error")
        );
        assert!(mock.calls.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(f.directory.path()).unwrap().count(), 1);
    }
}
#[tokio::test]
async fn tampering_fails_without_credentials() {
    let f = Fixture::new(offline());
    f.write(&saved_state()).await;
    let mut envelope: Value = serde_json::from_slice(&f.raw()).unwrap();
    let mut bytes = STANDARD
        .decode(envelope["ciphertext"].as_str().unwrap())
        .unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    envelope["ciphertext"] = STANDARD.encode(bytes).into();
    f.write_raw(&serde_json::to_vec(&envelope).unwrap());
    assert_eq!(
        f.store
            .lock(&CancellationToken::new())
            .await
            .unwrap()
            .read()
            .await
            .unwrap_err()
            .code,
        "storage_decryption_failed"
    );
}
#[tokio::test]
async fn signout_removes_tokens_even_when_remote_revocation_fails() {
    let f = Fixture::new(offline());
    f.write(&saved_state()).await;
    assert_eq!(
        f.client.disconnect().await.unwrap_err().code,
        "revocation_failed"
    );
    let p = f.read().await["profiles"][0].clone();
    assert!(p.get("credentials").is_none());
    assert!(p.get("profileIdToken").is_none());
    assert_eq!(p["status"], "disconnected");
    assert_eq!(p["clientId"], "registered_test_client");
    assert_eq!(
        serde_json::from_slice::<Value>(&f.raw()).unwrap()["version"],
        3
    );
}
#[cfg(unix)]
#[tokio::test]
async fn owner_only_files_and_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new(offline());
    f.write(&saved_state()).await;
    let filename = f.directory.path().join("chatgpt-auth.json");
    std::fs::set_permissions(&filename, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        f.store
            .lock(&CancellationToken::new())
            .await
            .unwrap()
            .read()
            .await
            .unwrap_err()
            .code,
        "storage_unsafe"
    );
    std::fs::remove_file(&filename).unwrap();
    let target = f.directory.path().join("target.json");
    std::fs::write(&target, serde_json::to_vec(&saved_state()).unwrap()).unwrap();
    symlink(&target, &filename).unwrap();
    assert_eq!(
        f.store
            .lock(&CancellationToken::new())
            .await
            .unwrap()
            .read()
            .await
            .unwrap_err()
            .code,
        "storage_invalid"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(target).unwrap()).unwrap()["version"],
        2
    );
}
#[tokio::test]
async fn lock_capability_and_compromise_are_enforced() {
    let f = Fixture::new(offline());
    let lock = f.store.lock(&CancellationToken::new()).await.unwrap();
    std::fs::remove_dir(f.directory.path().join(".chatgpt-auth.lock")).unwrap();
    assert_eq!(lock.read().await.unwrap_err().code, "storage_lock_lost");
    assert_eq!(
        lock.write(&saved_state()).await.unwrap_err().code,
        "storage_lock_lost"
    );
    // 未获得 LockedStore 就无法调用读写，编译期约束替代 JS 的无锁方法
}
#[tokio::test]
async fn duplicate_profiles_and_invalid_receipts_are_rejected_before_write() {
    let f = Fixture::new(offline());
    f.write(&saved_state()).await;
    let before = f.raw();
    let mut state = saved_state();
    let duplicate = state["profiles"][0].clone();
    state["profiles"].as_array_mut().unwrap().push(duplicate);
    assert_eq!(
        f.store
            .lock(&CancellationToken::new())
            .await
            .unwrap()
            .write(&state)
            .await
            .unwrap_err()
            .code,
        "storage_invalid"
    );
    assert_eq!(f.raw(), before);
}
