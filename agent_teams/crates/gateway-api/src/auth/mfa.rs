//! 双重验证 HTTP 合同，仅消费管理员会话用例

use axum::{
    Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use gateway_admin::model::auth::MfaProof;
use serde::Deserialize;
use std::net::SocketAddr;

use super::{SessionState, clear_session_cookie};
use crate::{
    admin::{AdminEnvelope, AdminError, AdminJson, AdminResponse, wire::map_admin_service_error},
    session_cookie,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Proof {
    password: String,
    setup_id: Option<String>,
    #[serde(default)]
    code: String,
}

pub(super) fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/auth/mfa", get(status::<S>))
        .route("/api/auth/mfa/setup", post(setup::<S>))
        .route("/api/auth/mfa/confirm", post(confirm::<S>))
        .route("/api/auth/mfa/disable", post(disable::<S>))
}

async fn status<S: SessionState + Send + Sync>(
    State(state): State<S>,
    headers: HeaderMap,
) -> Result<Response, AdminError> {
    let data = state
        .admin_services()
        .auth()
        .mfa_status(session_cookie::value(&headers).as_deref())
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)).into_response())
}

async fn setup<S: SessionState + Send + Sync>(
    State(state): State<S>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AdminJson(proof): AdminJson<Proof>,
) -> Result<Response, AdminError> {
    let data = state
        .admin_services()
        .auth()
        .mfa_setup(
            session_cookie::value(&headers).as_deref(),
            &proof.password,
            peer.ip(),
        )
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)).into_response())
}

async fn confirm<S: SessionState + Send + Sync>(
    State(state): State<S>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AdminJson(proof): AdminJson<Proof>,
) -> Result<Response, AdminError> {
    state
        .admin_services()
        .auth()
        .mfa_confirm(
            session_cookie::value(&headers).as_deref(),
            MfaProof {
                setup_id: proof.setup_id,
                password: proof.password,
                code: proof.code,
            },
            peer.ip(),
        )
        .await
        .map_err(map_admin_service_error)?;
    clear_session_cookie(
        AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(serde_json::json!({"message": "双重验证已更新，请重新登录"})),
        )
        .into_response(),
        &headers,
    )
}

async fn disable<S: SessionState + Send + Sync>(
    State(state): State<S>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    AdminJson(proof): AdminJson<Proof>,
) -> Result<Response, AdminError> {
    state
        .admin_services()
        .auth()
        .mfa_disable(
            session_cookie::value(&headers).as_deref(),
            MfaProof {
                setup_id: proof.setup_id,
                password: proof.password,
                code: proof.code,
            },
            peer.ip(),
        )
        .await
        .map_err(map_admin_service_error)?;
    clear_session_cookie(
        AdminResponse::new(
            StatusCode::OK,
            AdminEnvelope::ok(serde_json::json!({"message": "双重验证已关闭，请重新登录"})),
        )
        .into_response(),
        &headers,
    )
}
