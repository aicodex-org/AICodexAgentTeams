//! 验证双重验证迁移、并发更新和审计回滚

use super::*;

#[tokio::test]
async fn mfa_compare_update_is_atomic_with_password_and_audit() {
    use gateway_admin::model::auth::MfaState;
    use gateway_store::postgres::{AdminSecurityAuditRepository, PgAdminSecurityAuditRepository};
    let Some(database) = super::super::TestDatabase::create("mfa_atomic").await else {
        return;
    };
    let repository = PgAdminSecurityAuditRepository::new(database.pool.clone());
    repository
        .create_password_hash_if_absent("admin", "old-hash")
        .await
        .unwrap();
    let initial = repository.load_mfa("admin").await.unwrap();
    assert_eq!(initial, MfaState::default());
    let next = MfaState {
        enabled: true,
        encrypted_secret: "test-ciphertext".into(),
        last_used_step: Some(42),
        ..initial.clone()
    };
    let audit = |id: &str| AdminAuditEvent {
        id: id.into(),
        actor_kind: AdminAuditActorKind::AdminSession,
        actor_admin_user_id: Some("admin".into()),
        actor_ref: "admin:admin".into(),
        admin_request_id: None,
        action: "admin.mfa_enabled".into(),
        entity_kind: "admin_user".into(),
        entity_ref: "admin".into(),
        config_revision: None,
        changed_fields: vec!["mfa".into()],
        created_at: Utc::now(),
    };
    let (first, second) = tokio::join!(
        repository.update_mfa(
            "admin",
            "old-hash",
            &initial,
            &next,
            Some("new-hash"),
            Some(audit("mfa-1"))
        ),
        repository.update_mfa(
            "admin",
            "old-hash",
            &initial,
            &next,
            Some("new-hash"),
            Some(audit("mfa-2"))
        ),
    );
    assert_ne!(first.unwrap(), second.unwrap());
    assert_eq!(repository.load_mfa("admin").await.unwrap(), next);
    assert_eq!(
        repository.password_hash("admin").await.unwrap().as_deref(),
        Some("new-hash")
    );
    let count: i64 = sqlx::query_scalar("select count(*) from admin_audit_events")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let mut invalid = audit("mfa-invalid");
    invalid.actor_admin_user_id = Some("missing-user".into());
    assert!(
        repository
            .update_mfa(
                "admin",
                "new-hash",
                &next,
                &initial,
                Some("invalid-hash"),
                Some(invalid)
            )
            .await
            .is_err()
    );
    assert_eq!(repository.load_mfa("admin").await.unwrap(), next);
    assert_eq!(
        repository.password_hash("admin").await.unwrap().as_deref(),
        Some("new-hash")
    );
    database.close().await;
}

#[tokio::test]
async fn mfa_migration_preserves_existing_administrator_password() {
    use gateway_store::postgres::{AdminSecurityAuditRepository, PgAdminSecurityAuditRepository};
    let Some(database) = super::super::TestDatabase::create_through("mfa_upgrade", 29).await else {
        return;
    };
    let repository = PgAdminSecurityAuditRepository::new(database.pool.clone());
    repository
        .create_password_hash_if_absent("admin", "existing-password-hash")
        .await
        .unwrap();
    super::super::TEST_MIGRATOR
        .run(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        repository.password_hash("admin").await.unwrap().as_deref(),
        Some("existing-password-hash")
    );
    assert_eq!(
        repository.load_mfa("admin").await.unwrap(),
        Default::default()
    );
    database.close().await;
}
