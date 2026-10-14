//! 管理员双重验证与密码变更的原子比较更新

use gateway_admin::model::auth::MfaState;

use super::{
    AdminAuditEvent, PgAdminSecurityAuditRepository, append_admin_audit_event_in_transaction,
};
use crate::{StoreResult, postgres_unavailable};

impl PgAdminSecurityAuditRepository {
    pub async fn load_mfa(&self, user: &str) -> StoreResult<MfaState> {
        let state: sqlx::types::Json<MfaState> =
            sqlx::query_scalar("select mfa from admin_users where id = $1")
                .bind(user)
                .fetch_one(&self.pool)
                .await
                .map_err(|source| postgres_unavailable("load administrator MFA", source))?;
        Ok(state.0)
    }

    pub async fn update_mfa(
        &self,
        user: &str,
        expected_hash: &str,
        expected: &MfaState,
        next: &MfaState,
        new_hash: Option<&str>,
        audit: Option<AdminAuditEvent>,
    ) -> StoreResult<bool> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|source| postgres_unavailable("begin administrator MFA", source))?;
        let changed = sqlx::query("update admin_users set mfa = $4, password_hash = coalesce($5, password_hash), updated_at = now() where id = $1 and password_hash = $2 and mfa = $3")
            .bind(user).bind(expected_hash).bind(sqlx::types::Json(expected))
            .bind(sqlx::types::Json(next)).bind(new_hash).execute(&mut *tx).await
            .map_err(|source| postgres_unavailable("update administrator MFA", source))?;
        if changed.rows_affected() == 0 {
            return Ok(false);
        }
        if let Some(event) = audit {
            append_admin_audit_event_in_transaction(&mut tx, event, None).await?;
        }
        tx.commit()
            .await
            .map_err(|source| postgres_unavailable("commit administrator MFA", source))?;
        Ok(true)
    }
}
