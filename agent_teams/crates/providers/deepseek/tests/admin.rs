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
async fn balance_refresh_rejects_bad_financial_fields_without_replacing_snapshot() {
    let server = MockServer::start().await;
    let valid = json!({"is_available":true,"balance_infos":[
        {"currency":"CNY","total_balance":"12345678901234567890.00000000001","granted_balance":"0.00000000001","topped_up_balance":"12345678901234567890.00000000000"},
        {"currency":"USD","total_balance":"0","granted_balance":"0","topped_up_balance":"0"}
    ]});
    Mock::given(path("/user/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(valid.clone()))
        .mount(&server)
        .await;
    let (provider, _, id) = fixture(&server.uri()).await;
    let original = provider
        .quota(ProviderQuotaRequest {
            account_id: id.clone(),
            refresh: true,
            rolling_usage: None,
        })
        .await
        .unwrap();
    assert_eq!(original.balances.len(), 2);
    assert_eq!(
        original.balances[0].total_balance,
        "12345678901234567890.00000000001"
    );
    for (field, invalid_value) in [
        ("currency", json!("EUR")),
        ("total_balance", json!("NaN")),
        ("total_balance", json!(12)),
        ("total_balance", json!("1e3")),
        ("granted_balance", json!(null)),
        ("topped_up_balance", json!("1.2.3")),
    ] {
        server.reset().await;
        let mut malformed = valid.clone();
        malformed["is_available"] = json!(false);
        malformed["balance_infos"][0][field] = invalid_value;
        Mock::given(path("/user/balance"))
            .respond_with(ResponseTemplate::new(200).set_body_json(malformed))
            .mount(&server)
            .await;
        assert!(
            provider
                .quota(ProviderQuotaRequest {
                    account_id: id.clone(),
                    refresh: true,
                    rolling_usage: None
                })
                .await
                .is_err()
        );
        let cached = provider
            .quota(ProviderQuotaRequest {
                account_id: id.clone(),
                refresh: false,
                rolling_usage: None,
            })
            .await
            .unwrap();
        assert_eq!(cached.balances, original.balances);
        assert!(!cached.limit_reached);
    }
    server.reset().await;
    Mock::given(path("/user/balance"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let error = provider
        .quota(ProviderQuotaRequest {
            account_id: id.clone(),
            refresh: true,
            rolling_usage: None,
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.kind(),
        gateway_admin::ports::provider::ProviderAdminErrorKind::BadGateway
    );
    server.reset().await;
    Mock::given(path("/user/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(64 * 1024 + 1)))
        .mount(&server)
        .await;
    assert!(
        provider
            .quota(ProviderQuotaRequest {
                account_id: id.clone(),
                refresh: true,
                rolling_usage: None
            })
            .await
            .is_err()
    );
    let cached = provider
        .quota(ProviderQuotaRequest {
            account_id: id,
            refresh: false,
            rolling_usage: None,
        })
        .await
        .unwrap();
    assert_eq!(cached.balances, original.balances);
}

#[tokio::test]
async fn balance_unavailable_keeps_currency_and_zero_or_negative_amounts() {
    let server = MockServer::start().await;
    Mock::given(path("/user/balance"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"is_available":false,"balance_infos":[{"currency":"CNY","total_balance":"-0.01","granted_balance":"0","topped_up_balance":"-0.01"}]})))
        .mount(&server).await;
    let (provider, _, id) = fixture(&server.uri()).await;
    let quota = provider
        .quota(ProviderQuotaRequest {
            account_id: id,
            refresh: true,
            rolling_usage: None,
        })
        .await
        .unwrap();
    assert!(quota.limit_reached);
    assert_eq!(quota.balances[0].total_balance, "-0.01");
    assert_eq!(quota.balances[0].granted_balance.as_deref(), Some("0"));
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

#[tokio::test]
async fn import_and_configuration_rotation_keep_account_pricing_schedule() {
    let (provider, store, id) = fixture("https://api.deepseek.com").await;
    let schedule = json!({"enabled":false,"timezone":"Europe/London","peak_weekdays":[1,3,5],
        "peak_windows":[{"start":"08:00","end":"13:00"}],"exclude_chinese_holidays":false});
    let imported = provider
        .prepare_import(PrepareCredentialImport {
            default_outbound_proxy: None,
            document: document(json!({"api_key":"synthetic-key","pricing_schedule":schedule})),
        })
        .await
        .unwrap();
    let material = imported.credentials[0]
        .provider_material
        .expose_to_provider()
        .expose_to_provider();
    assert_eq!(material["pricing_schedule"], schedule);
    let exported = provider
        .export_credentials(vec![ProviderExportCredentialInput {
            account: account_record(&store.account(&id).unwrap()),
            provider_material: imported.credentials[0].provider_material.clone(),
        }])
        .await
        .unwrap();
    let exported = exported.document.expose_to_provider().expose_to_provider();
    assert_eq!(exported["accounts"][0]["pricing_schedule"], schedule);
    let rotation = provider
        .prepare_rotation(PrepareCredentialRotation {
            account: account_record(&store.account(&id).unwrap()),
            provider_material: document(json!({"pricing_schedule":schedule})),
        })
        .await
        .unwrap();
    let material = rotation
        .facts()
        .provider_material
        .expose_to_provider()
        .expose_to_provider();
    assert_eq!(material["pricing_schedule"], schedule);
    assert_eq!(material["api_key"], "synthetic-key");
    assert!(rotation.facts().preserve_credential_state);
    assert!(rotation.facts().preserve_profile);
    let configuration = provider.account_configuration(&id).await.unwrap().unwrap();
    let configuration = configuration.expose_to_provider().expose_to_provider();
    assert_eq!(
        configuration["pricing_schedule"]["timezone"],
        "Asia/Shanghai"
    );
    assert_eq!(configuration["pricing_schedule"]["enabled"], true);
    assert!(!configuration.contains_key("api_key"));
}
