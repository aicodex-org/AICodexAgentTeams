//! 分组模型映射的 PostgreSQL 原子写入、版本检查与快照读取

use super::{append_admin_audit_event_in_transaction, bump_config_revision_in_transaction};
use crate::{
    ConflictKind, StoreError, StoreResult, admin_revision, admin_store_error, mutation_audit,
    postgres_unavailable,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_admin::{
    model::{MutationContext, audit::MutationAuditOperation, model_mappings::*},
    ports::{
        model_mappings::ModelMappingStore,
        store::{AdminStoreError, AdminStoreResult},
    },
};
use gateway_core::routing::{AccountGroupId, GroupModelMappingEntry, GroupModelMappingPolicy};
use sqlx::{PgConnection, PgPool};

const ENTITY: &str = "model mapping policy";
const SELECT: &str = "select p.id, p.group_id, g.name as group_name, p.enabled, p.remark, p.revision, p.created_at, p.updated_at,
 coalesce((select jsonb_agg(jsonb_build_object('protocol', e.protocol, 'requestedModel', e.requested_model, 'routeModel', e.route_model, 'targetProvider', e.target_provider) order by e.position) from group_model_mapping_entries e where e.policy_id = p.id), '[]'::jsonb) as mappings
 from group_model_mapping_policies p join account_groups g on g.id = p.group_id";

#[derive(Clone)]
pub struct PgModelMappingRepository {
    pool: PgPool,
}
impl PgModelMappingRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[derive(sqlx::FromRow)]
struct PolicyRow {
    id: String,
    group_id: String,
    group_name: String,
    enabled: bool,
    remark: Option<String>,
    revision: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    mappings: sqlx::types::Json<Vec<GroupModelMappingEntry>>,
}
impl PolicyRow {
    fn record(self) -> StoreResult<ModelMappingRecord> {
        let policy = GroupModelMappingPolicy {
            id: self.id,
            group_id: AccountGroupId::new(self.group_id).map_err(|_| invalid())?,
            enabled: self.enabled,
            revision: u64::try_from(self.revision).map_err(|_| invalid())?,
            mappings: self.mappings.0,
        };
        policy.validate().map_err(|_| invalid())?;
        Ok(ModelMappingRecord {
            policy,
            group_name: self.group_name,
            remark: self.remark,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}
fn invalid() -> StoreError {
    StoreError::InvalidData {
        source: None,
        entity: ENTITY,
        message: "invalid model mapping policy".into(),
    }
}
fn unavailable(source: sqlx::Error) -> StoreError {
    postgres_unavailable("model mapping policy", source)
}
fn mapped(error: StoreError) -> AdminStoreError {
    admin_store_error(ENTITY, error)
}
fn conflict(id: &str) -> StoreError {
    StoreError::Conflict {
        source: None,
        entity: ENTITY,
        id: id.into(),
        kind: ConflictKind::StaleRevision,
    }
}
fn missing(id: &str) -> StoreError {
    StoreError::NotFound {
        source: None,
        entity: ENTITY,
        id: id.into(),
    }
}
fn write_error(error: sqlx::Error, id: &str) -> StoreError {
    if error
        .as_database_error()
        .is_some_and(|e| e.is_unique_violation())
    {
        StoreError::Conflict {
            source: Some(error.into()),
            entity: ENTITY,
            id: id.into(),
            kind: ConflictKind::InvalidTransition,
        }
    } else if error
        .as_database_error()
        .is_some_and(|e| e.is_foreign_key_violation() || e.is_check_violation())
    {
        invalid().with_source(error)
    } else {
        unavailable(error)
    }
}

async fn load_record(connection: &mut PgConnection, id: &str) -> StoreResult<ModelMappingRecord> {
    sqlx::QueryBuilder::new(SELECT)
        .push(" where p.id = ")
        .push_bind(id)
        .build_query_as::<PolicyRow>()
        .fetch_optional(connection)
        .await
        .map_err(unavailable)?
        .ok_or_else(|| missing(id))?
        .record()
}

pub(super) async fn load_snapshot_model_mappings(
    connection: &mut PgConnection,
) -> StoreResult<Vec<GroupModelMappingPolicy>> {
    sqlx::QueryBuilder::new(SELECT)
        .push(" order by p.id")
        .build_query_as::<PolicyRow>()
        .fetch_all(connection)
        .await
        .map_err(unavailable)?
        .into_iter()
        .map(|row| row.record().map(|r| r.policy))
        .collect()
}

#[async_trait]
impl ModelMappingStore for PgModelMappingRepository {
    async fn list(&self, query: ModelMappingListQuery) -> AdminStoreResult<ModelMappingPage> {
        if query.page == 0 {
            return Err(mapped(invalid()));
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| mapped(unavailable(e)))?;
        sqlx::query("set transaction isolation level repeatable read read only")
            .execute(&mut *tx)
            .await
            .map_err(|e| mapped(unavailable(e)))?;
        let search = query.search.as_deref().unwrap_or("");
        let count:i64 = sqlx::query_scalar("select count(*) from group_model_mapping_policies p join account_groups g on g.id = p.group_id where ($1::boolean is null or p.enabled = $1) and strpos(lower(g.name),lower($2)) > 0")
            .bind(query.enabled).bind(search).fetch_one(&mut *tx).await.map_err(|e|mapped(unavailable(e)))?;
        let offset = i64::from(query.page - 1) * i64::from(query.page_size.get());
        let items = sqlx::QueryBuilder::new(SELECT)
            .push(" where (")
            .push_bind(query.enabled)
            .push("::boolean is null or p.enabled = ")
            .push_bind(query.enabled)
            .push(") and strpos(lower(g.name),lower(")
            .push_bind(search)
            .push(")) > 0 order by p.created_at desc, p.id desc limit ")
            .push_bind(i64::from(query.page_size.get()))
            .push(" offset ")
            .push_bind(offset)
            .build_query_as::<PolicyRow>()
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| mapped(unavailable(e)))?
            .into_iter()
            .map(PolicyRow::record)
            .collect::<StoreResult<Vec<_>>>()
            .map_err(mapped)?;
        let revision: i64 =
            sqlx::query_scalar("select config_revision from runtime_settings where id=1")
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| mapped(unavailable(e)))?;
        tx.commit().await.map_err(|e| mapped(unavailable(e)))?;
        Ok(ModelMappingPage {
            items,
            total: u64::try_from(count).map_err(|_| mapped(invalid()))?,
            page: query.page,
            page_size: query.page_size.get(),
            config_revision: gateway_admin::model::Revision::new(
                u64::try_from(revision).map_err(|_| mapped(invalid()))?,
            )
            .map_err(|_| mapped(invalid()))?,
        })
    }
    async fn get(&self, id: &str) -> AdminStoreResult<ModelMappingRecord> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|e| mapped(unavailable(e)))?;
        load_record(&mut connection, id).await.map_err(mapped)
    }
    async fn save(
        &self,
        command: SaveModelMapping,
        context: &MutationContext,
    ) -> AdminStoreResult<ModelMappingMutation> {
        validate_save(&command).map_err(|_| mapped(invalid()))?;
        let id = command.policy.id.clone();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| mapped(unavailable(e)))?;
        let result: StoreResult<_> = async {
            // 所有配置写入先锁全局版本，沿用既有锁顺序，避免与分组删除交叉死锁
            let config_revision = bump_config_revision_in_transaction(&mut tx).await?;
            let operation = if let Some(expected) = command.expected_revision {
                let changed = sqlx::query("update group_model_mapping_policies set enabled=$1, remark=$2, revision=revision+1, updated_at=now() where id=$3 and group_id=$4 and revision=$5")
                    .bind(command.policy.enabled).bind(&command.remark).bind(&id).bind(command.policy.group_id.as_str()).bind(i64::try_from(expected).map_err(|_|invalid())?)
                    .execute(&mut *tx).await.map_err(|e|write_error(e,&id))?.rows_affected();
                if changed != 1 { return Err(conflict(&id)); }
                sqlx::query("delete from group_model_mapping_entries where policy_id=$1").bind(&id).execute(&mut *tx).await.map_err(unavailable)?;
                MutationAuditOperation::ModelMappingUpdate
            } else {
                sqlx::query("insert into group_model_mapping_policies (id,group_id,enabled,remark) values ($1,$2,$3,$4)")
                    .bind(&id).bind(command.policy.group_id.as_str()).bind(command.policy.enabled).bind(&command.remark)
                    .execute(&mut *tx).await.map_err(|e|write_error(e,&id))?;
                MutationAuditOperation::ModelMappingCreate
            };
            for (position,entry) in command.policy.mappings.iter().enumerate() {
                sqlx::query("insert into group_model_mapping_entries (policy_id,protocol,requested_model,route_model,target_provider,position) values ($1,$2,$3,$4,$5,$6)")
                    .bind(&id).bind(&entry.protocol).bind(&entry.requested_model).bind(&entry.route_model).bind(entry.target_provider.as_str()).bind(i32::try_from(position).map_err(|_|invalid())?)
                    .execute(&mut *tx).await.map_err(|e|write_error(e,&id))?;
            }
            append_admin_audit_event_in_transaction(&mut tx,mutation_audit(context,operation,&id,vec!["enabled".into(),"remark".into(),"mappings".into()]),config_revision).await?;
            let record = load_record(&mut tx,&id).await?;
            Ok((config_revision,record))
        }.await;
        match result {
            Ok((revision, record)) => {
                tx.commit().await.map_err(|e| mapped(unavailable(e)))?;
                Ok(ModelMappingMutation {
                    id,
                    record: Some(record),
                    config_revision: admin_revision(revision)?,
                })
            }
            Err(error) => {
                let error = match tx.rollback().await {
                    Ok(()) => error,
                    Err(cleanup) => error.with_cleanup(cleanup),
                };
                Err(mapped(error))
            }
        }
    }
    async fn delete(
        &self,
        id: &str,
        expected_revision: u64,
        context: &MutationContext,
    ) -> AdminStoreResult<ModelMappingMutation> {
        let expected = i64::try_from(expected_revision).map_err(|_| mapped(invalid()))?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| mapped(unavailable(e)))?;
        let result: StoreResult<_> = async {
            let revision = bump_config_revision_in_transaction(&mut tx).await?;
            let changed =
                sqlx::query("delete from group_model_mapping_policies where id=$1 and revision=$2")
                    .bind(id)
                    .bind(expected)
                    .execute(&mut *tx)
                    .await
                    .map_err(unavailable)?
                    .rows_affected();
            if changed != 1 {
                return Err(conflict(id));
            }
            append_admin_audit_event_in_transaction(
                &mut tx,
                mutation_audit(
                    context,
                    MutationAuditOperation::ModelMappingDelete,
                    id,
                    vec!["policy".into()],
                ),
                revision,
            )
            .await?;
            Ok(revision)
        }
        .await;
        match result {
            Ok(revision) => {
                tx.commit().await.map_err(|e| mapped(unavailable(e)))?;
                Ok(ModelMappingMutation {
                    id: id.into(),
                    record: None,
                    config_revision: admin_revision(revision)?,
                })
            }
            Err(error) => {
                let error = match tx.rollback().await {
                    Ok(()) => error,
                    Err(cleanup) => error.with_cleanup(cleanup),
                };
                Err(mapped(error))
            }
        }
    }
}
