//! 模型映射 HTTP 鉴权与精确模型合同测试

use async_trait::async_trait;
use gateway_admin::{
    model::{MutationContext, model_mappings::*},
    ports::{
        model_mappings::ModelMappingStore,
        store::{AdminStoreError, AdminStoreErrorKind, AdminStoreResult},
    },
};

pub(super) struct UnavailableMappings;
#[async_trait]
impl ModelMappingStore for UnavailableMappings {
    async fn list(&self, _: ModelMappingListQuery) -> AdminStoreResult<ModelMappingPage> {
        Err(unavailable())
    }
    async fn get(&self, _: &str) -> AdminStoreResult<ModelMappingRecord> {
        Err(unavailable())
    }
    async fn save(
        &self,
        _: SaveModelMapping,
        _: &MutationContext,
    ) -> AdminStoreResult<ModelMappingMutation> {
        Err(unavailable())
    }
    async fn delete(
        &self,
        _: &str,
        _: u64,
        _: &MutationContext,
    ) -> AdminStoreResult<ModelMappingMutation> {
        Err(unavailable())
    }
}
fn unavailable() -> AdminStoreError {
    AdminStoreError::new(AdminStoreErrorKind::Unavailable, "model mapping", "unused")
}

#[tokio::test]
async fn model_mapping_requires_auth_and_rejects_wildcard_before_storage() {
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt as _;
    let fixture = super::AdminTestFixture::new().await;
    let app = gateway_api::admin::model_mappings::router::<super::AdminTestState>()
        .with_state(fixture.state());
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/model-mapping-policies")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    fixture.auth.insert_session("valid-session");
    for invalid in ["all", "*", "model-*", "a,b", ""] {
        let body = serde_json::json!({"groupId":super::PRIMARY_GROUP_ID,"enabled":true,"remark":null,"mappings":[{"protocol":"openai-chat","requestedModel":invalid,"routeModel":"deepseek-chat","targetProvider":"deepseek"}]});
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .header("x-request-id", "req_model_mapping")
                    .uri("/api/admin/model-mapping-policies/create")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::COOKIE, "aicodex_agent_teams_session=valid-session")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{invalid}");
    }
}
