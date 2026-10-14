//! totp-rs 适配与高熵恢复码，不实现 TOTP 算法

use rand_core::{OsRng, RngCore as _};
use sha2::{Digest, Sha256};

use crate::model::AdminError;

pub(super) fn new_totp(user: &str) -> Result<totp_rs::Totp, AdminError> {
    totp_rs::Builder::new()
        .with_issuer(Some("AICodexAgentTeams"))
        .with_account_name(user)
        .build()
        .map_err(|_| AdminError::internal("验证器参数无效"))
}

pub(super) fn recovery_codes() -> Vec<String> {
    (0..10)
        .map(|_| {
            let mut bytes = [0_u8; 16];
            OsRng.fill_bytes(&mut bytes);
            hex::encode(bytes)
        })
        .collect()
}

pub(super) fn recovery_hash(code: &str) -> String {
    hex::encode(Sha256::digest(code.as_bytes()))
}

pub(super) fn matched_step(secret: &str, code: &str, now: i64) -> Result<Option<u64>, AdminError> {
    let secret = totp_rs::Secret::try_from_base32(secret)
        .map_err(|_| AdminError::internal("验证器密钥无效"))?;
    let totp = totp_rs::Builder::new()
        .with_secret(secret)
        .build()
        .map_err(|_| AdminError::internal("验证器参数无效"))?;
    let time = u64::try_from(now).map_err(|_| AdminError::internal("服务器时间无效"))?;
    Ok(totp.check(code, time))
}
