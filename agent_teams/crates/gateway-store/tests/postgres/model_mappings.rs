//! 验证分组映射的唯一性、原子替换、并发版本、快照与删除级联

use super::TestDatabase;
use gateway_admin::{
    model::{MutationActor, MutationContext, PageSize, model_mappings::*},
    ports::{model_mappings::ModelMappingStore, store::AdminStoreErrorKind},
};
use gateway_core::routing::{
    AccountGroupId, GroupModelMappingEntry, GroupModelMappingPolicy, ProviderKind,
};
use gateway_store::postgres::{
    PgModelMappingRepository, PgRuntimeSnapshotRepository, RuntimeSnapshotRepository,
};

const GROUP: &str = "grp_00000000000000000000000000000001";
fn command(id: &str) -> SaveModelMapping {
    SaveModelMapping {
        policy: GroupModelMappingPolicy {
            id: id.into(),
            group_id: AccountGroupId::new(GROUP).unwrap(),
            enabled: true,
            revision: 1,
            mappings: vec![GroupModelMappingEntry {
                protocol: "openai-chat".into(),
                requested_model: "coding".into(),
                route_model: "deepseek-chat".into(),
                target_provider: ProviderKind::new("deepseek").unwrap(),
            }],
        },
        remark: Some("开发组".into()),
        expected_revision: None,
    }
}
fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "mapping-test".into(),
    }
}

#[tokio::test]
async fn mapping_policy_is_atomic_unique_versioned_and_loaded_in_snapshot() {
    let Some(db) = TestDatabase::create("model_mapping_policy").await else {
        return;
    };
    sqlx::query("insert into account_groups (id,name,color,created_at,updated_at) values ($1,'开发组','#2563EBFF',now(),now())").bind(GROUP).execute(&db.pool).await.unwrap();
    let repo = PgModelMappingRepository::new(db.pool.clone());
    let created = repo.save(command("policy-a"), &context()).await.unwrap();
    assert_eq!(created.record.as_ref().unwrap().policy.revision, 1);
    let mut duplicate = command("policy-b");
    duplicate.policy.enabled = false;
    assert_eq!(
        repo.save(duplicate, &context()).await.unwrap_err().kind(),
        AdminStoreErrorKind::Conflict
    );
    let before = created.config_revision;
    let mut update = command("policy-a");
    update.expected_revision = Some(1);
    update.policy.mappings[0].route_model = "deepseek-reasoner".into();
    let updated = repo.save(update.clone(), &context()).await.unwrap();
    assert_eq!(updated.record.as_ref().unwrap().policy.revision, 2);
    assert_eq!(updated.config_revision.get(), before.get() + 1);
    assert_eq!(
        repo.save(update, &context()).await.unwrap_err().kind(),
        AdminStoreErrorKind::StaleRevision
    );
    let mut invalid = command("policy-a");
    invalid.expected_revision = Some(2);
    invalid
        .policy
        .mappings
        .push(invalid.policy.mappings[0].clone());
    assert_eq!(
        repo.save(invalid, &context()).await.unwrap_err().kind(),
        AdminStoreErrorKind::Invalid
    );
    let record = repo.get("policy-a").await.unwrap();
    assert_eq!(record.policy.revision, 2);
    assert_eq!(record.policy.mappings.len(), 1);
    assert_eq!(record.policy.mappings[0].route_model, "deepseek-reasoner");
    let snap = PgRuntimeSnapshotRepository::new(db.pool.clone())
        .load_runtime_snapshot()
        .await
        .unwrap();
    assert_eq!(snap.model_mapping_policies, vec![record.policy]);
    assert_eq!(snap.config_revision.get(), updated.config_revision.get());
    let page = repo
        .list(ModelMappingListQuery {
            page: 1,
            page_size: PageSize::new(10).unwrap(),
            search: Some("开发".into()),
            enabled: Some(true),
        })
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].group_name, "开发组");
    assert_eq!(
        repo.delete("policy-a", 1, &context())
            .await
            .unwrap_err()
            .kind(),
        AdminStoreErrorKind::StaleRevision
    );
    repo.delete("policy-a", 2, &context()).await.unwrap();
    repo.save(command("policy-b"), &context()).await.unwrap();
    sqlx::query("delete from account_groups where id=$1")
        .bind(GROUP)
        .execute(&db.pool)
        .await
        .unwrap();
    let entries: i64 = sqlx::query_scalar("select count(*) from group_model_mapping_entries")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(entries, 0);
    db.close().await;
}

#[tokio::test]
async fn concurrent_policy_updates_have_one_winner_without_partial_entries() {
    let Some(db) = TestDatabase::create("model_mapping_concurrency").await else {
        return;
    };
    sqlx::query("insert into account_groups (id,name,color,created_at,updated_at) values ($1,'并发组','#2563EBFF',now(),now())").bind(GROUP).execute(&db.pool).await.unwrap();
    let repo = PgModelMappingRepository::new(db.pool.clone());
    repo.save(command("policy-a"), &context()).await.unwrap();
    let mut first = command("policy-a");
    first.expected_revision = Some(1);
    first.policy.mappings[0].route_model = "first-model".into();
    let mut second = first.clone();
    second.policy.mappings[0].route_model = "second-model".into();
    let ctx = context();
    let (a, b) = tokio::join!(repo.save(first, &ctx), repo.save(second, &ctx));
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        a.err().or(b.err()).unwrap().kind(),
        AdminStoreErrorKind::StaleRevision
    );
    assert_eq!(repo.get("policy-a").await.unwrap().policy.revision, 2);
    db.close().await;
}
