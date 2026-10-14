//! 双重验证绑定、一次性证明与安全变更，复用管理员会话和密码校验

use std::net::IpAddr;

use chrono::Utc;

use super::{
    AuthService, DefaultAuthService, LOGIN_ATTEMPTS_GLOBAL, LOGIN_ATTEMPTS_PER_SOURCE,
    LOGIN_WINDOW, hash_admin_password, password_fingerprint, secret, totp, verify_admin_password,
};
use crate::model::{
    AdminError, AdminErrorKind,
    auth::{MfaProof, MfaSetup, MfaState, MfaStatus, SessionSubject},
};
use crate::use_case::map_store_error;

impl DefaultAuthService {
    pub(super) async fn mfa_limit(&self, user: &str) -> Result<(), AdminError> {
        if let Some(retry_after) = self
            .store
            .consume_mfa_attempt(user, 10, LOGIN_WINDOW)
            .await
            .map_err(|error| map_store_error(error, "MFA account limit"))?
        {
            return Err(AdminError::new(
                AdminErrorKind::RateLimited,
                format!(
                    "尝试过于频繁，请在 {} 秒后重试",
                    retry_after.as_secs().max(1)
                ),
            ));
        }
        Ok(())
    }

    async fn mfa_admin(&self, session_id: Option<&str>) -> Result<String, AdminError> {
        self.resolve_admin_user_id(session_id)
            .await?
            .ok_or_else(|| AdminError::new(AdminErrorKind::Unauthorized, "请先登录"))
    }

    async fn mfa_credentials(
        &self,
        session_id: Option<&str>,
        password: &str,
        source: IpAddr,
    ) -> Result<(String, String, MfaState), AdminError> {
        if self
            .store
            .consume_login_attempt(
                source,
                LOGIN_ATTEMPTS_PER_SOURCE,
                LOGIN_ATTEMPTS_GLOBAL,
                LOGIN_WINDOW,
            )
            .await
            .map_err(|error| map_store_error(error, "MFA rate limit"))?
            .is_some()
        {
            return Err(AdminError::new(
                AdminErrorKind::RateLimited,
                "尝试过于频繁，请稍后再试",
            ));
        }
        let session = self
            .session(session_id)
            .await?
            .ok_or_else(|| AdminError::new(AdminErrorKind::Unauthorized, "请先登录"))?;
        let SessionSubject::Admin {
            admin_user_id: user,
            credential_fingerprint,
        } = session.subject
        else {
            return Err(AdminError::new(
                AdminErrorKind::Forbidden,
                "仅管理员可以管理双重验证",
            ));
        };
        let hash = self
            .store
            .load_password_hash(&user)
            .await
            .map_err(|error| map_store_error(error, "administrator"))?
            .filter(|hash| password_fingerprint(hash) == credential_fingerprint)
            .ok_or_else(|| AdminError::conflict("登录状态已变更，请重新登录"))?;
        if password.len() > 4096 || !verify_admin_password(password, &hash)? {
            return Err(AdminError::invalid("当前密码不正确"));
        }
        self.mfa_limit(&user).await?;
        let mfa = self
            .store
            .load_mfa(&user)
            .await
            .map_err(|error| map_store_error(error, "administrator MFA"))?;
        Ok((user, hash, mfa))
    }

    pub(super) async fn mfa_status_impl(
        &self,
        session: Option<&str>,
    ) -> Result<MfaStatus, AdminError> {
        let user = self.mfa_admin(session).await?;
        let mfa = self
            .store
            .load_mfa(&user)
            .await
            .map_err(|error| map_store_error(error, "administrator MFA"))?;
        Ok(MfaStatus {
            enabled: mfa.enabled,
            recovery_codes_remaining: mfa.recovery_hashes.len(),
        })
    }

    pub(super) async fn mfa_setup_impl(
        &self,
        session: Option<&str>,
        password: &str,
        source: IpAddr,
    ) -> Result<MfaSetup, AdminError> {
        let (user, hash, previous) = self.mfa_credentials(session, password, source).await?;
        let codes = totp::recovery_codes();
        let setup_id = super::random_session_token();
        let mut next = previous.clone();
        next.setup_id = setup_id.clone();
        next.setup_expires_at = Utc::now().timestamp() + 600;
        next.pending_recovery_hashes = codes.iter().map(|code| totp::recovery_hash(code)).collect();
        let (secret_text, qr_code) = if previous.enabled {
            (String::new(), String::new())
        } else {
            let totp = totp::new_totp(&user)?;
            let text = totp.secret().to_base32();
            let qr = totp
                .to_qr_base64()
                .map_err(|_| AdminError::internal("生成验证器二维码失败"))?;
            next.encrypted_secret = secret::seal(&text, password, &user).await?;
            (text, qr)
        };
        self.save_mfa(&user, &hash, &previous, &next, None, None)
            .await?;
        Ok(MfaSetup {
            secret: secret_text,
            qr_code,
            setup_id,
            recovery_codes: codes,
        })
    }

    pub(super) async fn mfa_confirm_impl(
        &self,
        session: Option<&str>,
        proof: MfaProof,
        source: IpAddr,
    ) -> Result<(), AdminError> {
        let (user, hash, previous) = self
            .mfa_credentials(session, &proof.password, source)
            .await?;
        if previous.setup_id.is_empty()
            || proof.setup_id.as_deref() != Some(previous.setup_id.as_str())
            || previous.encrypted_secret.is_empty()
            || previous.setup_expires_at <= Utc::now().timestamp()
        {
            return Err(AdminError::conflict("绑定已失效，请重新开始"));
        }
        let mut next = if previous.enabled {
            verify_factor(&previous, &proof.password, &user, &proof.code).await?
        } else {
            let secret = secret::open(&previous.encrypted_secret, &proof.password, &user).await?;
            let step = totp::matched_step(&secret, &proof.code, Utc::now().timestamp())?
                .ok_or_else(|| AdminError::invalid("验证码不正确或已过期"))?;
            MfaState {
                last_used_step: Some(step),
                ..previous.clone()
            }
        };
        next.enabled = true;
        next.setup_id.clear();
        next.setup_expires_at = 0;
        next.recovery_hashes = std::mem::take(&mut next.pending_recovery_hashes);
        // 同一密码重新加盐，使所有既有会话与尚未完成的登录失效
        let next_hash = hash_admin_password(&proof.password)?;
        let action = if previous.enabled {
            "admin.mfa_recovery_regenerated"
        } else {
            "admin.mfa_enabled"
        };
        self.save_mfa(
            &user,
            &hash,
            &previous,
            &next,
            Some(&next_hash),
            Some(action),
        )
        .await
    }

    pub(super) async fn mfa_disable_impl(
        &self,
        session: Option<&str>,
        proof: MfaProof,
        source: IpAddr,
    ) -> Result<(), AdminError> {
        let (user, hash, previous) = self
            .mfa_credentials(session, &proof.password, source)
            .await?;
        if !previous.enabled {
            return Err(AdminError::conflict("双重验证未开启"));
        }
        verify_factor(&previous, &proof.password, &user, &proof.code).await?;
        let next_hash = hash_admin_password(&proof.password)?;
        self.save_mfa(
            &user,
            &hash,
            &previous,
            &MfaState::default(),
            Some(&next_hash),
            Some("admin.mfa_disabled"),
        )
        .await
    }

    pub(super) async fn save_mfa(
        &self,
        user: &str,
        hash: &str,
        previous: &MfaState,
        next: &MfaState,
        new_hash: Option<&str>,
        action: Option<&str>,
    ) -> Result<(), AdminError> {
        let audit = action.map(|action| {
            let mut audit = self.auth_audit(action, user);
            audit.entity_kind = "admin_user".to_owned();
            audit.changed_fields = vec!["mfa".to_owned()];
            audit
        });
        if !self
            .store
            .update_mfa(user, hash, previous, next, new_hash, audit)
            .await
            .map_err(|error| map_store_error(error, "administrator MFA"))?
        {
            return Err(AdminError::conflict("验证状态已变更，请重试"));
        }
        Ok(())
    }
}

pub(super) async fn verify_factor(
    previous: &MfaState,
    password: &str,
    user: &str,
    code: &str,
) -> Result<MfaState, AdminError> {
    let mut next = previous.clone();
    if code.len() == 32 && code.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let digest = totp::recovery_hash(code);
        let index = next
            .recovery_hashes
            .iter()
            .position(|hash| hash == &digest)
            .ok_or_else(|| AdminError::invalid("恢复码不正确或已使用"))?;
        next.recovery_hashes.remove(index);
    } else {
        let secret = secret::open(&previous.encrypted_secret, password, user).await?;
        let step = totp::matched_step(&secret, code, Utc::now().timestamp())?
            .filter(|step| previous.last_used_step.is_none_or(|last| *step > last))
            .ok_or_else(|| AdminError::invalid("验证码不正确、已过期或已使用"))?;
        next.last_used_step = Some(step);
    }
    Ok(next)
}
