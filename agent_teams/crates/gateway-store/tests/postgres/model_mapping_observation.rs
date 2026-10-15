//! 验证映射历史的可空平台合同及入口错误旧消息兼容

use super::{TestDatabase, observability_repository};
use chrono::{TimeDelta, Utc};
use gateway_store::postgres::{
    ObservabilityPageSize, ObservabilityRange, OpsErrorFilter, OpsErrorQuery, OpsEvent,
    OpsEventLevel, OpsEventRepository, PgOpsEventRepository,
};
use serde_json::json;

#[tokio::test]
async fn global_mapping_requires_null_target_and_preserves_legacy_observation() {
    let Some(db) = TestDatabase::create("global_mapping_shape").await else {
        return;
    };
    let base = json!({"request":{"configRevision":1,"compact":false,"protocol":"openai","endpoint":"/v1/responses"},"routing":{"scope":"all","groupRefs":[],"groupNamesSnapshot":[]}});
    let validate = |document| async {
        sqlx::query_scalar::<_, bool>("select model_request_observation_valid($1)")
            .bind(sqlx::types::Json(document))
            .fetch_one(&db.pool)
            .await
            .unwrap()
    };
    assert!(validate(base.clone()).await, "旧记录不需要映射字段");
    let mapping = json!({"source":"global","protocol":"openai","requestedModel":"coding","routeModel":"model-target","targetProvider":null,"policies":[]});
    let mut document = base.clone();
    document["routing"]["modelMapping"] = mapping;
    assert!(validate(document.clone()).await, "全局映射不固定平台");
    document["routing"]["modelMapping"]["targetProvider"] = json!("openai");
    assert!(
        !validate(document.clone()).await,
        "不能把平台固定伪装成全局改名"
    );
    document["routing"]["modelMapping"]["source"] = json!("extension");
    assert!(validate(document.clone()).await, "扩展别名明确固定平台");
    document["routing"]["modelMapping"]["targetProvider"] = serde_json::Value::Null;
    assert!(
        !validate(document.clone()).await,
        "扩展别名不能省略目标平台"
    );
    document["routing"]["modelMapping"]["source"] = json!("group");
    document["routing"]["modelMapping"]["targetProvider"] = json!("openai");
    assert!(
        !validate(document.clone()).await,
        "分组映射必须携带策略快照"
    );
    db.close().await;
}

#[tokio::test]
async fn plain_text_and_invalid_json_entry_errors_remain_queryable() {
    let Some(db) = TestDatabase::create("entry_mapping_legacy").await else {
        return;
    };
    let store = PgOpsEventRepository::new(db.pool.clone());
    for (index, message) in ["plain rejection", "{\"modelMapping\":", "null", "{}"]
        .into_iter()
        .enumerate()
    {
        store
            .append_ops_event(OpsEvent {
                id: format!("entry_{index}"),
                model_request_id: None,
                attempt_index: None,
                level: OpsEventLevel::Warning,
                component: "request_entry".into(),
                operation: "reject".into(),
                provider_kind: None,
                account_snapshot: None,
                provider_account_ref: None,
                upstream_model_id: None,
                failure_kind: "invalid_request".into(),
                upstream_send_state: None,
                error_details: None,
                status_code: None,
                provider_error_code: None,
                retry_after_ms: None,
                upstream_request_id: None,
                latency_ms: Some(1),
                message: message.into(),
                created_at: Utc::now(),
            })
            .await
            .unwrap();
    }
    let now = Utc::now();
    let page = observability_repository(&db.pool)
        .list_ops_errors(OpsErrorQuery {
            range: ObservabilityRange::new(
                now - TimeDelta::minutes(1),
                now + TimeDelta::minutes(1),
            )
            .unwrap(),
            filter: OpsErrorFilter::default(),
            current_page: 1,
            page_size: ObservabilityPageSize::new(10).unwrap(),
        })
        .await
        .unwrap();
    assert_eq!(page.total, 4);
    assert!(page.items.iter().all(|item| item.model_mapping.is_none()));
    db.close().await;
}
