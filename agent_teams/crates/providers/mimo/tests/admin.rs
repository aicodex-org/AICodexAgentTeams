//! MiMo 账号导入、轮换、目录与无余额查询边界验证
use crate::support::{account_record, fixture};
use gateway_admin::{model::provider_credentials::*, ports::provider::ProviderAdmin};
use gateway_core::account::OpaqueProviderData;
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};
fn document(value: serde_json::Value) -> ProviderDocument {
    ProviderDocument::new(OpaqueProviderData::new(value.as_object().unwrap().clone()))
}
#[tokio::test]
async fn import_keeps_one_account_for_three_protocols_and_exports() {
    let (provider, _, _) = fixture("https://api.xiaomimimo.com/v1").await;
    let imported = provider
        .prepare_import(PrepareCredentialImport {
            default_outbound_proxy: None,
            document: document(json!({"accounts":[{"name":"MiMo","api_key":"synthetic-key"}]})),
        })
        .await
        .unwrap();
    assert_eq!(imported.credentials.len(), 1);
    let material = imported.credentials[0]
        .provider_material
        .expose_to_provider()
        .expose_to_provider();
    assert_eq!(material["protocols"].as_array().unwrap().len(), 3);
}
#[tokio::test]
async fn rotation_preserves_connection_and_uses_current_revision() {
    let (provider, store, id) = fixture("https://api.xiaomimimo.com/v1").await;
    let rotation = provider
        .prepare_rotation(PrepareCredentialRotation {
            account: account_record(&store.account(&id).unwrap()),
            provider_material: document(json!({"api_key":"replacement-key"})),
        })
        .await
        .unwrap();
    let material = rotation
        .facts()
        .provider_material
        .expose_to_provider()
        .expose_to_provider();
    assert_eq!(material["api_key"], "replacement-key");
    assert_eq!(material["base_url"], "https://api.xiaomimimo.com/v1");
    assert!(!rotation.facts().preserve_credential_state);
    let config = provider.account_configuration(&id).await.unwrap().unwrap();
    assert!(
        !config
            .expose_to_provider()
            .expose_to_provider()
            .contains_key("api_key")
    );
}
#[tokio::test]
async fn model_discovery_preserves_account_source_and_uses_cache() {
    use gateway_core::engine::provider::Provider;
    let server = MockServer::start().await;
    Mock::given(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"mimo-v2.6-pro"},{"id":"future-model"}]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let (provider, _, id) = fixture(&server.uri()).await;
    let models = provider.models(&id, false).await.unwrap();
    assert_eq!(models.models.len(), 2);
    let capabilities = provider.query_model_capabilities().await.unwrap();
    assert_eq!(capabilities.len(), 2);
    assert!(
        capabilities
            .iter()
            .all(|model| model.catalog_accounts().unwrap().contains(&id))
    );
}

#[tokio::test]
async fn quota_is_unsupported_without_network_or_credential_state_mutation() {
    let server = MockServer::start().await;
    let (provider, store, id) = fixture(&server.uri()).await;
    let before = store.account(&id).unwrap();
    let capabilities = provider.account_capabilities(&id, "api_key");
    assert!(!capabilities.quota);
    assert!(!capabilities.quota_refresh);
    for refresh in [false, true] {
        let result = provider
            .quota(ProviderQuotaRequest {
                account_id: id.clone(),
                refresh,
                rolling_usage: None,
            })
            .await;
        assert_eq!(
            result.unwrap_err().kind(),
            gateway_admin::ports::provider::ProviderAdminErrorKind::Unsupported
        );
    }
    assert_eq!(store.account(&id).unwrap(), before);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn token_plan_import_export_and_configuration_rotation_preserve_mode_and_key() {
    let (provider, store, id) = fixture("https://token-plan-sgp.xiaomimimo.com/v1").await;
    let imported = provider
        .prepare_import(PrepareCredentialImport {
            default_outbound_proxy: None,
            document: document(json!({"api_key":"ttp-synthetic", "protocols":["openai-chat"]})),
        })
        .await
        .unwrap();
    let imported_material = imported.credentials[0]
        .provider_material
        .expose_to_provider()
        .expose_to_provider();
    assert_eq!(imported_material["access_mode"], "token_plan");
    assert_eq!(
        imported_material["base_url"],
        "https://token-plan-cn.xiaomimimo.com/v1"
    );
    assert_eq!(
        imported.credentials[0].credential_state,
        gateway_admin::model::accounts::CredentialState::Ready
    );
    let rotation = provider
        .prepare_rotation(PrepareCredentialRotation {
            account: account_record(&store.account(&id).unwrap()),
            provider_material: document(
                json!({"access_mode":"token_plan", "protocols":["anthropic"]}),
            ),
        })
        .await
        .unwrap();
    let material = rotation
        .facts()
        .provider_material
        .expose_to_provider()
        .expose_to_provider();
    assert_eq!(material["api_key"], "synthetic-key");
    assert_eq!(material["access_mode"], "token_plan");
    assert_eq!(
        material["base_url"],
        "https://token-plan-sgp.xiaomimimo.com/v1"
    );
    assert!(rotation.facts().preserve_credential_state);
}

#[tokio::test]
async fn export_import_round_trip_preserves_generic_model_access_policy() {
    use gateway_core::account::{AccountModelAccess, AccountModelAccessMode};
    let (provider, store, id) = fixture("https://api.xiaomimimo.com/v1").await;
    let policy = AccountModelAccess::new(
        AccountModelAccessMode::Allowlist,
        vec!["mimo-v2.6-pro".to_owned()],
    )
    .unwrap();
    let mut account = account_record(&store.account(&id).unwrap());
    account.model_access = policy.clone();
    let exported=provider.export_credentials(vec![ProviderExportCredentialInput {
        account,
        provider_material:document(json!({"api_key":"synthetic-key","access_mode":"token_plan","base_url":"https://token-plan-ams.xiaomimimo.com/v1","protocols":["anthropic"]})),
    }]).await.unwrap();
    let imported = provider
        .prepare_import(PrepareCredentialImport {
            default_outbound_proxy: None,
            document: exported.document,
        })
        .await
        .unwrap();
    assert_eq!(imported.credentials[0].model_access, Some(policy));
    let material = imported.credentials[0]
        .provider_material
        .expose_to_provider()
        .expose_to_provider();
    assert!(
        !material.contains_key("modelAccess"),
        "generic account policy must not become credential state"
    );
    assert_eq!(material["access_mode"], "token_plan");
    assert_eq!(material["protocols"], json!(["anthropic"]));
    assert!(provider.prepare_import(PrepareCredentialImport {default_outbound_proxy:None,document:document(json!({"api_key":"synthetic","modelAccess":{"mode":"all","models":["mimo-v2.6-pro"]}}))}).await.is_err());
}
