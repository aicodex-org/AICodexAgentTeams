//! 账号导入、密钥轮换、模型目录与币种余额持久快照验证
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
    let (provider, _, _) = fixture("https://api.deepseek.com").await;
    let imported = provider
        .prepare_import(PrepareCredentialImport {
            default_outbound_proxy: None,
            document: document(json!({"accounts":[{"name":"DeepSeek","api_key":"synthetic-key"}]})),
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
    let (provider, store, id) = fixture("https://api.deepseek.com").await;
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
    assert_eq!(material["base_url"], "https://api.deepseek.com");
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
async fn balance_keeps_decimal_currency_and_persists_for_nonrefresh_reads() {
    let server = MockServer::start().await;
    Mock::given(path("/user/balance")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"10.001","granted_balance":"0.001","topped_up_balance":"10.000"}]}))).expect(1).mount(&server).await;
    let (provider, _, id) = fixture(&server.uri()).await;
    let value = provider
        .quota(ProviderQuotaRequest {
            account_id: id.clone(),
            refresh: true,
            rolling_usage: None,
        })
        .await
        .unwrap();
    assert_eq!(value.balances[0].total_balance, "10.001");
    assert_eq!(value.balances[0].currency, "CNY");
    assert!(value.credits.is_none());
    let cached = provider
        .quota(ProviderQuotaRequest {
            account_id: id,
            refresh: false,
            rolling_usage: None,
        })
        .await
        .unwrap();
    assert_eq!(cached.balances, value.balances);
}
#[tokio::test]
async fn model_discovery_preserves_account_source_and_uses_cache() {
    use gateway_core::engine::provider::Provider;
    let server = MockServer::start().await;
    Mock::given(path("/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data":[{"id":"deepseek-flash"},{"id":"future-model"}]})),
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
