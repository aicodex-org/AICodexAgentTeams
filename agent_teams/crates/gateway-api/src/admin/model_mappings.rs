//! 分组模型映射策略的固定管理路由与 HTTP 投影

use super::{
    AdminAuth, AdminEnvelope, AdminError, AdminJson, AdminQuery, AdminResponse, PageMeta,
    wire::map_admin_service_error,
};
use crate::auth::SessionState;
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use gateway_admin::model::{PageSize, model_mappings::*};
use gateway_core::routing::{AccountGroupId, GroupModelMappingEntry, GroupModelMappingPolicy};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListQuery {
    page: Option<u32>,
    page_size: Option<u16>,
    search: Option<String>,
    enabled: Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IdQuery {
    id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OptionsQuery {
    group_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateRequest {
    group_id: String,
    enabled: bool,
    remark: Option<String>,
    mappings: Vec<GroupModelMappingEntry>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateRequest {
    id: String,
    expected_revision: u64,
    enabled: bool,
    remark: Option<String>,
    mappings: Vec<GroupModelMappingEntry>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevisionRequest {
    id: String,
    expected_revision: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PolicyView {
    id: String,
    group_id: String,
    group_name: String,
    enabled: bool,
    remark: Option<String>,
    revision: u64,
    mappings: Vec<GroupModelMappingEntry>,
    created_at: chrono::DateTime<chrono::Utc>,
    created_at_display: String,
    updated_at: chrono::DateTime<chrono::Utc>,
    updated_at_display: String,
}
impl PolicyView {
    fn new(r: ModelMappingRecord, time: crate::time::TimePresenter) -> Self {
        Self {
            id: r.policy.id,
            group_id: r.policy.group_id.to_string(),
            group_name: r.group_name,
            enabled: r.policy.enabled,
            remark: r.remark,
            revision: r.policy.revision,
            mappings: r.policy.mappings,
            created_at_display: time.datetime(&r.created_at),
            updated_at_display: time.datetime(&r.updated_at),
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PageData {
    items: Vec<PolicyView>,
    page: PageMeta,
    config_revision: u64,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MutationData {
    id: String,
    record: Option<PolicyView>,
    config_revision: u64,
}
#[derive(Serialize)]
struct TargetView {
    protocol: String,
    provider: String,
}
#[derive(Serialize)]
struct OptionsData {
    targets: Vec<TargetView>,
}

pub fn router<S>() -> Router<S>
where
    S: SessionState + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/admin/model-mapping-policies", get(list::<S>))
        .route("/api/admin/model-mapping-policies/detail", get(detail::<S>))
        .route(
            "/api/admin/model-mapping-policies/options",
            get(options::<S>),
        )
        .route(
            "/api/admin/model-mapping-policies/create",
            post(create::<S>),
        )
        .route(
            "/api/admin/model-mapping-policies/preview",
            post(preview::<S>),
        )
        .route(
            "/api/admin/model-mapping-policies/update",
            post(update::<S>),
        )
        .route(
            "/api/admin/model-mapping-policies/enable",
            post(enable::<S>),
        )
        .route(
            "/api/admin/model-mapping-policies/disable",
            post(disable::<S>),
        )
        .route(
            "/api/admin/model-mapping-policies/delete",
            post(delete::<S>),
        )
}
async fn list<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(q): AdminQuery<ListQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let page = q.page.unwrap_or(1);
    let size = q.page_size.unwrap_or(20);
    if page == 0 || size == 0 || size > 200 {
        return Err(AdminError::bad_request("分页参数不合法"));
    }
    let result = state
        .admin_services()
        .model_mappings()
        .list(ModelMappingListQuery {
            page,
            page_size: PageSize::new(size)
                .map_err(|_| AdminError::bad_request("分页参数不合法"))?,
            search: q.search,
            enabled: q.enabled,
        })
        .await
        .map_err(map_admin_service_error)?;
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PageData {
            items: result
                .items
                .into_iter()
                .map(|r| PolicyView::new(r, time))
                .collect(),
            page: PageMeta::new(
                result.page,
                u32::from(result.page_size),
                result.total,
                u32::try_from(result.total.div_ceil(u64::from(result.page_size)))
                    .unwrap_or(u32::MAX),
            ),
            config_revision: result.config_revision.get(),
        }),
    ))
}
async fn detail<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(q): AdminQuery<IdQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let r = state
        .admin_services()
        .model_mappings()
        .get(&q.id)
        .await
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PolicyView::new(
            r,
            crate::time::TimePresenter::new(state.admin_services().timezone()),
        )),
    ))
}
async fn options<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminQuery(q): AdminQuery<OptionsQuery>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let group =
        AccountGroupId::new(q.group_id).map_err(|_| AdminError::bad_request("分组 ID 不合法"))?;
    let targets = state
        .admin_services()
        .model_mappings()
        .targets(&group)
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(OptionsData {
            targets: targets
                .into_iter()
                .map(|(protocol, provider)| TargetView {
                    protocol,
                    provider: provider.to_string(),
                })
                .collect(),
        }),
    ))
}
async fn create<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(r): AdminJson<CreateRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let command = SaveModelMapping {
        policy: GroupModelMappingPolicy {
            id: String::new(),
            group_id: AccountGroupId::new(r.group_id)
                .map_err(|_| AdminError::bad_request("分组 ID 不合法"))?,
            enabled: r.enabled,
            revision: 1,
            mappings: r.mappings,
        },
        remark: r.remark,
        expected_revision: None,
    };
    response(
        &state,
        StatusCode::CREATED,
        state
            .admin_services()
            .model_mappings()
            .save(&auth.context().mutation_context(), command)
            .await,
    )
}
async fn update<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(r): AdminJson<UpdateRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let existing = state
        .admin_services()
        .model_mappings()
        .get(&r.id)
        .await
        .map_err(map_admin_service_error)?;
    let command = SaveModelMapping {
        policy: GroupModelMappingPolicy {
            id: r.id,
            group_id: existing.policy.group_id,
            enabled: r.enabled,
            revision: r.expected_revision,
            mappings: r.mappings,
        },
        remark: r.remark,
        expected_revision: Some(r.expected_revision),
    };
    response(
        &state,
        StatusCode::OK,
        state
            .admin_services()
            .model_mappings()
            .save(&auth.context().mutation_context(), command)
            .await,
    )
}
async fn enable<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(r): AdminJson<RevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    set_enabled(auth, state, r, true).await
}
async fn disable<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(r): AdminJson<RevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    set_enabled(auth, state, r, false).await
}
async fn set_enabled<S>(
    auth: AdminAuth,
    state: S,
    r: RevisionRequest,
    enabled: bool,
) -> Result<AdminResponse<AdminEnvelope<MutationData>>, AdminError>
where
    S: SessionState + Send + Sync,
{
    response(
        &state,
        StatusCode::OK,
        state
            .admin_services()
            .model_mappings()
            .set_enabled(
                &auth.context().mutation_context(),
                &r.id,
                r.expected_revision,
                enabled,
            )
            .await,
    )
}
async fn delete<S>(
    auth: AdminAuth,
    State(state): State<S>,
    AdminJson(r): AdminJson<RevisionRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    response(
        &state,
        StatusCode::OK,
        state
            .admin_services()
            .model_mappings()
            .delete(
                &auth.context().mutation_context(),
                &r.id,
                r.expected_revision,
            )
            .await,
    )
}
fn response<S>(
    state: &S,
    status: StatusCode,
    result: Result<ModelMappingMutation, gateway_admin::model::AdminError>,
) -> Result<AdminResponse<AdminEnvelope<MutationData>>, AdminError>
where
    S: SessionState,
{
    let r = result.map_err(map_admin_service_error)?;
    let time = crate::time::TimePresenter::new(state.admin_services().timezone());
    Ok(AdminResponse::new(
        status,
        AdminEnvelope::ok(MutationData {
            id: r.id,
            record: r.record.map(|r| PolicyView::new(r, time)),
            config_revision: r.config_revision.get(),
        }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewRequest {
    group_id: String,
    policy_id: Option<String>,
    revision: Option<u64>,
    enabled: bool,
    mappings: Vec<GroupModelMappingEntry>,
    client_key_id: String,
    protocol: String,
    requested_model: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewData {
    config_revision: u64,
    model_mapping: Option<gateway_core::routing::ModelMappingTrace>,
    targets: Vec<PreviewTarget>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewTarget {
    provider: String,
    route_model: String,
}
async fn preview<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    AdminJson(r): AdminJson<PreviewRequest>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    let policy = GroupModelMappingPolicy {
        id: r.policy_id.unwrap_or_else(|| "preview".into()),
        group_id: AccountGroupId::new(r.group_id)
            .map_err(|_| AdminError::bad_request("分组 ID 不合法"))?,
        enabled: r.enabled,
        revision: r.revision.unwrap_or(1),
        mappings: r.mappings,
    };
    let key = gateway_core::policy::ClientApiKeyId::new(r.client_key_id)
        .map_err(|_| AdminError::bad_request("API 密钥 ID 不合法"))?;
    let result = state
        .admin_services()
        .model_mappings()
        .preview(policy, key, r.protocol, r.requested_model)
        .map_err(map_admin_service_error)?;
    Ok(AdminResponse::new(
        StatusCode::OK,
        AdminEnvelope::ok(PreviewData {
            config_revision: result.config_revision,
            model_mapping: result.model_mapping,
            targets: result
                .targets
                .into_iter()
                .map(|(provider, route_model)| PreviewTarget {
                    provider,
                    route_model,
                })
                .collect(),
        }),
    ))
}
