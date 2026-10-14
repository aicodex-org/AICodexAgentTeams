//! 双重验证的绑定、恢复、防重放、并发和改密生命周期

use super::MemoryAuthStore;
use chrono::Utc;
use gateway_admin::{
    AdminServices,
    model::{
        AdminErrorKind,
        auth::{ChangePassword, LoginCommand, LoginError, MfaProof, MfaSetup},
    },
};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};

const PASSWORD: &str = "strong-test-password";
fn source() -> IpAddr {
    Ipv4Addr::LOCALHOST.into()
}
fn command(password: &str, code: Option<&str>) -> LoginCommand {
    LoginCommand::Admin {
        username: None,
        password: password.into(),
        second_factor: code.map(str::to_owned),
    }
}
fn proof(code: &str, setup_id: Option<&str>) -> MfaProof {
    MfaProof {
        password: PASSWORD.into(),
        code: code.into(),
        setup_id: setup_id.map(str::to_owned),
    }
}
fn token(setup: &MfaSetup, offset: i64) -> String {
    totp_rs::Builder::new()
        .with_secret(totp_rs::Secret::try_from_base32(&setup.secret).unwrap())
        .build()
        .unwrap()
        .generate((Utc::now().timestamp() + offset) as u64)
        .to_string()
}
async fn fixture() -> (AdminServices, Arc<MemoryAuthStore>, String) {
    let store = Arc::new(MemoryAuthStore::default());
    let services = super::super::AdminHarness::new()
        .auth(store.clone())
        .build()
        .await;
    let auth = services.auth();
    let session = auth
        .login(command(PASSWORD, None), source(), None)
        .await
        .unwrap();
    (services, store, session.session_id)
}
async fn enabled() -> (AdminServices, Arc<MemoryAuthStore>, MfaSetup, String) {
    let (services, store, session) = fixture().await;
    let auth = services.auth();
    let setup = auth
        .mfa_setup(Some(&session), PASSWORD, source())
        .await
        .unwrap();
    auth.mfa_confirm(
        Some(&session),
        proof(&token(&setup, 0), Some(&setup.setup_id)),
        source(),
    )
    .await
    .unwrap();
    (services, store, setup, session)
}

#[tokio::test]
async fn enrollment_requires_matching_setup_and_code_and_revokes_old_sessions() {
    let (services, store, session) = fixture().await;
    let auth = services.auth();
    let first = auth
        .mfa_setup(Some(&session), PASSWORD, source())
        .await
        .unwrap();
    let second = auth
        .mfa_setup(Some(&session), PASSWORD, source())
        .await
        .unwrap();
    assert!(!second.qr_code.is_empty());
    assert_eq!(second.recovery_codes.len(), 10);
    assert!(
        !serde_json::to_string(&*store.mfa.lock().unwrap())
            .unwrap()
            .contains(&second.secret)
    );
    assert!(
        auth.mfa_confirm(
            Some(&session),
            proof(&token(&first, 0), Some(&first.setup_id)),
            source()
        )
        .await
        .is_err()
    );
    assert!(
        auth.mfa_confirm(
            Some(&session),
            proof("invalid", Some(&second.setup_id)),
            source()
        )
        .await
        .is_err()
    );
    assert!(auth.session(Some(&session)).await.unwrap().is_some());
    assert!(!auth.mfa_status(Some(&session)).await.unwrap().enabled);
    auth.mfa_confirm(
        Some(&session),
        proof(&token(&second, 0), Some(&second.setup_id)),
        source(),
    )
    .await
    .unwrap();
    assert!(auth.session(Some(&session)).await.unwrap().is_none());
    assert_eq!(
        auth.login(command(PASSWORD, None), source(), None)
            .await
            .unwrap_err(),
        LoginError::SecondFactorRequired
    );
    assert!(
        auth.login(command("wrong-password", None), source(), None)
            .await
            .is_err()
    );
    assert!(
        auth.login(command(PASSWORD, Some("invalid")), source(), None)
            .await
            .is_err()
    );
    let login = auth
        .login(
            command(PASSWORD, Some(&second.recovery_codes[0])),
            source(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        auth.mfa_status(Some(&login.session_id))
            .await
            .unwrap()
            .recovery_codes_remaining,
        9
    );
}

#[tokio::test]
async fn recovery_code_and_totp_are_consumed_once_under_concurrent_logins() {
    let (services, _, setup, _) = enabled().await;
    let auth = services.auth();
    let code = &setup.recovery_codes[0];
    let (first, second) = tokio::join!(
        auth.login(command(PASSWORD, Some(code)), source(), None),
        auth.login(command(PASSWORD, Some(code)), source(), None)
    );
    assert_ne!(first.is_ok(), second.is_ok());
    assert!(
        auth.login(
            command(PASSWORD, Some(&token(&setup, -120))),
            source(),
            None
        )
        .await
        .is_err()
    );
    let code = token(&setup, 30);
    let (first, second) = tokio::join!(
        auth.login(command(PASSWORD, Some(&code)), source(), None),
        auth.login(command(PASSWORD, Some(&code)), source(), None)
    );
    assert_ne!(first.is_ok(), second.is_ok());
    assert!(
        auth.login(command(PASSWORD, Some(&code)), source(), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn password_change_reencrypts_secret_without_disabling_mfa() {
    let (services, store, setup, _) = enabled().await;
    let auth = services.auth();
    let before = store.mfa.lock().unwrap().encrypted_secret.clone();
    let login = auth
        .login(
            command(PASSWORD, Some(&setup.recovery_codes[0])),
            source(),
            None,
        )
        .await
        .unwrap();
    let new_password = "new-strong-test-password";
    auth.change_password(
        Some(&login.session_id),
        ChangePassword {
            current_password: PASSWORD.into(),
            new_password: new_password.into(),
        },
        source(),
    )
    .await
    .unwrap();
    assert_ne!(before, store.mfa.lock().unwrap().encrypted_secret);
    assert!(
        auth.session(Some(&login.session_id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        auth.login(
            command(PASSWORD, Some(&setup.recovery_codes[1])),
            source(),
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        auth.login(command(new_password, None), source(), None)
            .await
            .unwrap_err(),
        LoginError::SecondFactorRequired
    );
    assert!(
        auth.login(
            command(new_password, Some(&token(&setup, 30))),
            source(),
            None
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn recovery_rotation_preserves_existing_codes_until_confirmation() {
    let (services, _, setup, _) = enabled().await;
    let auth = services.auth();
    let login = auth
        .login(
            command(PASSWORD, Some(&setup.recovery_codes[0])),
            source(),
            None,
        )
        .await
        .unwrap();
    let pending = auth
        .mfa_setup(Some(&login.session_id), PASSWORD, source())
        .await
        .unwrap();
    assert!(pending.secret.is_empty() && pending.qr_code.is_empty());
    assert!(
        auth.login(
            command(PASSWORD, Some(&pending.recovery_codes[0])),
            source(),
            None
        )
        .await
        .is_err()
    );
    assert!(
        auth.login(
            command(PASSWORD, Some(&setup.recovery_codes[1])),
            source(),
            None
        )
        .await
        .is_ok()
    );
    auth.mfa_confirm(
        Some(&login.session_id),
        proof(&setup.recovery_codes[2], Some(&pending.setup_id)),
        source(),
    )
    .await
    .unwrap();
    assert!(
        auth.session(Some(&login.session_id))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        auth.login(
            command(PASSWORD, Some(&setup.recovery_codes[3])),
            source(),
            None
        )
        .await
        .is_err()
    );
    assert!(
        auth.login(
            command(PASSWORD, Some(&pending.recovery_codes[0])),
            source(),
            None
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn disable_requires_full_proof_and_revokes_sessions() {
    let (services, store, setup, _) = enabled().await;
    let auth = services.auth();
    let login = auth
        .login(
            command(PASSWORD, Some(&setup.recovery_codes[0])),
            source(),
            None,
        )
        .await
        .unwrap();
    assert!(
        auth.mfa_disable(None, proof(&setup.recovery_codes[1], None), source())
            .await
            .is_err()
    );
    assert!(
        auth.mfa_disable(Some(&login.session_id), proof("bad", None), source())
            .await
            .is_err()
    );
    assert!(
        auth.mfa_status(Some(&login.session_id))
            .await
            .unwrap()
            .enabled
    );
    auth.mfa_disable(
        Some(&login.session_id),
        proof(&setup.recovery_codes[1], None),
        source(),
    )
    .await
    .unwrap();
    assert!(
        auth.session(Some(&login.session_id))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(*store.mfa.lock().unwrap(), Default::default());
    assert!(
        auth.login(command(PASSWORD, None), source(), None)
            .await
            .is_ok()
    );
    assert!(
        store
            .audits
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.action == "admin.mfa_disabled")
    );
}

#[tokio::test]
async fn expired_setup_and_rate_limit_leave_security_state_unchanged() {
    let (services, store, session) = fixture().await;
    let auth = services.auth();
    let setup = auth
        .mfa_setup(Some(&session), PASSWORD, source())
        .await
        .unwrap();
    store.mfa.lock().unwrap().setup_expires_at = Utc::now().timestamp() - 1;
    assert!(
        auth.mfa_confirm(
            Some(&session),
            proof(&token(&setup, 0), Some(&setup.setup_id)),
            source()
        )
        .await
        .is_err()
    );
    *store.retry_after.lock().unwrap() = Some(std::time::Duration::from_secs(60));
    assert_eq!(
        auth.mfa_setup(Some(&session), PASSWORD, source())
            .await
            .err()
            .unwrap()
            .kind(),
        AdminErrorKind::RateLimited
    );
    assert!(!store.mfa.lock().unwrap().enabled);
}

#[tokio::test]
async fn corrupted_ciphertext_never_falls_back_to_password_only_login() {
    let (services, store, setup, _) = enabled().await;
    let auth = services.auth();
    store.mfa.lock().unwrap().encrypted_secret = "v1:invalid".into();
    assert_eq!(
        auth.login(command(PASSWORD, Some(&token(&setup, 30))), source(), None)
            .await
            .unwrap_err(),
        LoginError::Unavailable
    );
    assert_eq!(
        auth.login(command(PASSWORD, None), source(), None)
            .await
            .unwrap_err(),
        LoginError::SecondFactorRequired
    );
}

#[tokio::test]
async fn account_limit_rejects_recovery_proof_without_consuming_it() {
    let (services, store, setup, _) = enabled().await;
    let auth = services.auth();
    *store.mfa_retry_after.lock().unwrap() = Some(std::time::Duration::from_secs(60));
    assert!(matches!(
        auth.login(
            command(PASSWORD, Some(&setup.recovery_codes[0])),
            source(),
            None
        )
        .await,
        Err(LoginError::TooManyAttempts { .. })
    ));
    assert_eq!(store.mfa.lock().unwrap().recovery_hashes.len(), 10);
    *store.mfa_retry_after.lock().unwrap() = None;
    assert!(
        auth.login(
            command(PASSWORD, Some(&setup.recovery_codes[0])),
            source(),
            None
        )
        .await
        .is_ok()
    );
}
