//! 使用 ring 的 PBKDF2 与 AES-GCM 保护绑定密钥，耗时派生移出异步执行线程

use crate::model::AdminError;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore as _};
use ring::{aead, pbkdf2};
use std::num::NonZeroU32;

pub(super) async fn seal(secret: &str, password: &str, user: &str) -> Result<String, AdminError> {
    let secret = zeroize::Zeroizing::new(secret.to_owned());
    let password = zeroize::Zeroizing::new(password.to_owned());
    let user = user.to_owned();
    tokio::task::spawn_blocking(move || seal_blocking(&secret, &password, &user))
        .await
        .map_err(|_| AdminError::internal("双重验证加密任务失败"))?
}

pub(super) async fn open(
    encoded: &str,
    password: &str,
    user: &str,
) -> Result<zeroize::Zeroizing<String>, AdminError> {
    let encoded = encoded.to_owned();
    let password = zeroize::Zeroizing::new(password.to_owned());
    let user = user.to_owned();
    tokio::task::spawn_blocking(move || {
        open_blocking(&encoded, &password, &user).map(zeroize::Zeroizing::new)
    })
    .await
    .map_err(|_| AdminError::internal("双重验证解密任务失败"))?
}

fn wrapping_key(password: &str, salt: &[u8]) -> Result<aead::LessSafeKey, AdminError> {
    let mut key = zeroize::Zeroizing::new([0_u8; 32]);
    let iterations =
        NonZeroU32::new(600_000).ok_or_else(|| AdminError::internal("密钥派生参数无效"))?;
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        password.as_bytes(),
        key.as_mut(),
    );
    aead::UnboundKey::new(&aead::AES_256_GCM, key.as_ref())
        .map(aead::LessSafeKey::new)
        .map_err(|_| AdminError::internal("双重验证密钥初始化失败"))
}

fn seal_blocking(secret: &str, password: &str, user: &str) -> Result<String, AdminError> {
    let mut salt = [0_u8; 16];
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);
    let key = wrapping_key(password, &salt)?;
    let mut body = secret.as_bytes().to_vec();
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(user.as_bytes()),
        &mut body,
    )
    .map_err(|_| AdminError::internal("双重验证密钥加密失败"))?;
    let mut encoded = Vec::from(salt);
    encoded.extend_from_slice(&nonce);
    encoded.extend(body);
    Ok(format!("v1:{}", URL_SAFE_NO_PAD.encode(encoded)))
}

fn open_blocking(encoded: &str, password: &str, user: &str) -> Result<String, AdminError> {
    let invalid = || AdminError::internal("双重验证密钥不可用");
    let mut bytes = zeroize::Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(encoded.strip_prefix("v1:").ok_or_else(invalid)?)
            .map_err(|_| invalid())?,
    );
    if bytes.len() < 44 {
        return Err(invalid());
    }
    let key = wrapping_key(password, &bytes[..16])?;
    let nonce: [u8; 12] = bytes[16..28].try_into().map_err(|_| invalid())?;
    let plaintext = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(user.as_bytes()),
            &mut bytes[28..],
        )
        .map_err(|_| invalid())?;
    String::from_utf8(plaintext.to_vec()).map_err(|_| invalid())
}
