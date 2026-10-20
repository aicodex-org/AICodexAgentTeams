//! TypeSafe API Key 导入、轮换、目录与连接测试准备

use crate::{
    credential::{TypeSafeCredential, document, invalid, unavailable},
    provider::TypeSafeProvider,
    transport,
};
use async_trait::async_trait;
use chrono::Utc;
use gateway_admin::{
    model::{
        accounts::{CredentialState, ProviderAccountCapabilities},
        observability::{CalculatedBillingBreakdown, DashboardWireProfile, ProviderBillingInput},
        provider_credentials::*,
    },
    ports::provider::{ProviderAdmin, ProviderAdminError, ProviderAdminErrorKind},
};
use gateway_core::{
    account::ProviderAccountId,
    operation::{Operation, RawJsonPayload, StructuredInferenceRequest},
    routing::{ProviderKind, UpstreamModelId},
};
use serde_json::{Value, json};

struct CommitGuard;
impl CredentialCommitGuard for CommitGuard {
    fn finish(self: Box<Self>) {}
}
fn unsupported() -> ProviderAdminError {
    ProviderAdminError::new(ProviderAdminErrorKind::Unsupported)
}

impl TypeSafeProvider {
    async fn admin_material(
        &self,
        account: &ProviderAccountId,
    ) -> Result<(TypeSafeCredential, gateway_core::account::ProviderAccount), ProviderAdminError>
    {
        let loaded = self
            .ports
            .accounts()
            .load_current_credential(account)
            .await
            .map_err(|_| unavailable())?;
        if loaded.account.provider() != &self.kind {
            return Err(invalid());
        }
        Ok((
            TypeSafeCredential::parse(loaded.credential.expose_to_provider())?,
            loaded.account,
        ))
    }
    async fn admin_get(
        &self,
        account: &ProviderAccountId,
        path: &str,
    ) -> Result<(Value, gateway_core::account::ProviderAccount), ProviderAdminError> {
        let (credential, account) = self.admin_material(account).await?;
        let client = transport::client(account.outbound_proxy()).map_err(|_| unavailable())?;
        let response = client
            .get(format!("{}{path}", credential.base_url))
            .bearer_auth(&credential.api_key)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::BadGateway));
        }
        let limit = 4 * 1024 * 1024;
        let bytes = transport::bounded_body(response, limit)
            .await
            .map_err(|_| unavailable())?;
        if transport::contains_secret(&bytes, &credential.api_key) {
            return Err(unavailable());
        }
        Ok((
            crate::schema::parse_json(&bytes)
                .map_err(|_| ProviderAdminError::new(ProviderAdminErrorKind::BadGateway))?,
            account,
        ))
    }
}

#[async_trait]
impl ProviderAdmin for TypeSafeProvider {
    fn pricing_catalog(&self) -> gateway_admin::model::pricing::ProviderPricingCatalog {
        crate::pricing::catalog()
    }
    fn provider_kind(&self) -> &ProviderKind {
        &self.kind
    }
    fn account_capabilities(&self, _: &ProviderAccountId, _: &str) -> ProviderAccountCapabilities {
        ProviderAccountCapabilities {
            quota: false,
            quota_refresh: false,
            ..Default::default()
        }
    }
    async fn account_unavailable(&self, _: &ProviderAccountId) {}
    fn dashboard_wire_profile(&self) -> Option<DashboardWireProfile> {
        None
    }
    fn calculated_billing(
        &self,
        _: &ProviderBillingInput,
    ) -> Result<Option<CalculatedBillingBreakdown>, ProviderAdminError> {
        Ok(None)
    }
    async fn connection_test_operation(
        &self,
        model: &UpstreamModelId,
        input: &str,
    ) -> Result<Operation, ProviderAdminError> {
        self.connection_test_operation_for_protocol(model, input, Some("typesafe-systemone"))
            .await
    }
    async fn connection_test_operation_for_protocol(
        &self,
        model: &UpstreamModelId,
        input: &str,
        protocol: Option<&str>,
    ) -> Result<Operation, ProviderAdminError> {
        if protocol.is_some_and(|p| p != "typesafe-systemone") {
            return Err(unsupported());
        }
        let body = serde_json::to_vec(&json!({"model":model.as_str(), "state":input,
            "questions":{"connection_test":{"type":"noul","instructions":"Is this a connection test?"}}})).map_err(|_| invalid())?;
        Ok(Operation::StructuredInference(
            StructuredInferenceRequest::from_raw_json(
                RawJsonPayload::new("typesafe-systemone", bytes::Bytes::from(body))
                    .map_err(|_| invalid())?,
            ),
        ))
    }
    async fn prepare_import(
        &self,
        command: PrepareCredentialImport,
    ) -> Result<PreparedCredentialImport, ProviderAdminError> {
        let root = command.document.expose_to_provider().expose_to_provider();
        let values = match root.get("accounts") {
            Some(Value::Array(values)) => values.clone(),
            Some(_) => return Err(invalid()),
            None => vec![Value::Object(root.clone())],
        };
        if values.is_empty() || values.len() > 1000 {
            return Err(invalid());
        }
        let mut credentials = Vec::with_capacity(values.len());
        for value in values {
            let object = value.as_object().ok_or_else(invalid)?;
            let credential = TypeSafeCredential::parse(object)?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Jev")
                .trim();
            if name.is_empty() || name.len() > 255 {
                return Err(invalid());
            }
            credentials.push(PreparedCredentialCreate {
                model_access: None,
                outbound_proxy: command.default_outbound_proxy.clone(),
                account_id: ProviderAccountId::new(format!("acct_{}", uuid::Uuid::new_v4()))
                    .map_err(|_| invalid())?,
                provider_kind: self.kind.clone(),
                name: name.to_owned(),
                email: None,
                upstream_user_id: None,
                upstream_account_id: None,
                plan_type: None,
                authentication_kind: "api_key".to_owned(),
                provider_material: credential.document()?,
                has_refresh_token: false,
                access_token_expires_at: None,
                next_refresh_at: None,
                enabled: true,
                credential_state: CredentialState::Ready,
                credential_observed_at: Utc::now(),
            });
        }
        Ok(PreparedCredentialImport {
            provider_kind: self.kind.clone(),
            credentials,
        })
    }
    async fn start_authorization(
        &self,
        _: PendingAuthorizationMutation,
    ) -> Result<AuthorizationStarted, ProviderAdminError> {
        Err(unsupported())
    }
    async fn complete_authorization(
        &self,
        _: CompleteAuthorization,
    ) -> Result<PreparedAuthorizationCommit, ProviderAdminError> {
        Err(unsupported())
    }
    async fn prepare_refresh(
        &self,
        _: PrepareCredentialRefresh,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        Err(unsupported())
    }
    async fn prepare_rotation(
        &self,
        command: PrepareCredentialRotation,
    ) -> Result<PreparedCredentialRotation, ProviderAdminError> {
        if command.account.provider_kind != self.kind {
            return Err(invalid());
        }
        let account_id =
            ProviderAccountId::new(command.account.id.clone()).map_err(|_| invalid())?;
        let loaded = self
            .ports
            .accounts()
            .load_current_credential(&account_id)
            .await
            .map_err(|_| unavailable())?;
        if loaded.account.provider() != &self.kind {
            return Err(invalid());
        }
        let mut merged = loaded.credential.expose_to_provider().clone();
        let incoming = command
            .provider_material
            .expose_to_provider()
            .expose_to_provider();
        let key_changed = incoming
            .get("api_key")
            .is_some_and(|key| merged.get("api_key") != Some(key));
        for (key, value) in incoming {
            merged.insert(key.clone(), value.clone());
        }
        let credential = TypeSafeCredential::parse(&merged)?;
        Ok(PreparedCredentialRotation::new(
            PreparedCredentialRotationFacts {
                account_id,
                provider_kind: self.kind.clone(),
                expected_credential_revision: gateway_admin::model::Revision::new(
                    loaded.account.revision().get(),
                )
                .map_err(|_| invalid())?,
                replacement_identity: None,
                name: command.account.name,
                email: None,
                plan_type: None,
                preserve_profile: true,
                preserve_credential_state: !key_changed,
                credential_state: key_changed.then_some(CredentialState::Ready),
                provider_material: credential.document()?,
                has_refresh_token: false,
                access_token_expires_at: None,
                next_refresh_at: None,
            },
            Box::new(CommitGuard),
        ))
    }
    async fn account_configuration(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<ProviderDocument>, ProviderAdminError> {
        Ok(Some(self.admin_material(account).await?.0.configuration()?))
    }
    async fn quota(
        &self,
        request: ProviderQuotaRequest,
    ) -> Result<ProviderQuota, ProviderAdminError> {
        let _ = request;
        Err(unsupported())
    }
    async fn models(
        &self,
        account: &ProviderAccountId,
        refresh: bool,
    ) -> Result<ProviderModels, ProviderAdminError> {
        let (_, current) = self.admin_material(account).await?;
        let key = gateway_core::provider_ports::ProviderCatalogCacheKey::new(
            self.kind.clone(),
            gateway_core::provider_ports::ProviderCatalogScope::new(format!(
                "{}:{}",
                account.as_str(),
                current.revision().get()
            ))
            .map_err(|_| invalid())?,
        );
        let cached = if refresh {
            None
        } else {
            self.ports
                .catalog_cache()
                .read(&key)
                .await
                .map_err(|_| unavailable())?
        };
        let value = if let Some(cached) = cached {
            Value::Object(cached.expose_to_provider().clone())
        } else {
            let (value, loaded) = self.admin_get(account, "/v1/models").await?;
            if loaded.revision() != current.revision() {
                return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict));
            }
            parse_models(&value)?;
            self.ports
                .catalog_cache()
                .replace(
                    &key,
                    &document(value.clone())?.into_provider_data(),
                    std::time::Duration::from_secs(300),
                )
                .await
                .map_err(|_| unavailable())?;
            value
        };
        let models = parse_models(&value)?;
        Ok(ProviderModels {
            models,
            observed_at: Some(Utc::now()),
        })
    }
    async fn export_credentials(
        &self,
        credentials: Vec<ProviderExportCredentialInput>,
    ) -> Result<ProviderExport, ProviderAdminError> {
        let mut ids = Vec::new();
        let mut accounts = Vec::new();
        for entry in credentials {
            if entry.account.provider_kind != self.kind {
                return Err(invalid());
            }
            ids.push(ProviderAccountId::new(entry.account.id).map_err(|_| invalid())?);
            let credential = TypeSafeCredential::parse(
                entry
                    .provider_material
                    .expose_to_provider()
                    .expose_to_provider(),
            )?;
            let mut value = serde_json::to_value(credential).map_err(|_| invalid())?;
            value["name"] = Value::String(entry.account.name);
            accounts.push(value);
        }
        Ok(ProviderExport {
            provider_kind: self.kind.clone(),
            account_ids: ids,
            document: document(json!({"accounts":accounts}))?,
        })
    }
}

// 上游目录采用 models/name 原生结构，重复或错误条目不能作为有效快照缓存
fn parse_models(value: &Value) -> Result<Vec<ProviderModel>, ProviderAdminError> {
    let entries = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(unavailable)?;
    let mut seen = std::collections::BTreeSet::new();
    entries
        .iter()
        .map(|entry| {
            let id = entry
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(unavailable)?;
            if !seen.insert(id) {
                return Err(unavailable());
            }
            Ok(ProviderModel {
                id: UpstreamModelId::new(id).map_err(|_| unavailable())?,
                name: id.to_owned(),
            })
        })
        .collect()
}
