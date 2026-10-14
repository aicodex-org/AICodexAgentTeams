//! DeepSeek API Key 导入、轮换、目录、余额与连接测试准备

use crate::{
    credential::{DeepSeekCredential, document, invalid, unavailable},
    provider::DeepSeekProvider,
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
    operation::{GenerateRequest, Operation, ProtocolPayload},
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

impl DeepSeekProvider {
    async fn admin_material(
        &self,
        account: &ProviderAccountId,
    ) -> Result<(DeepSeekCredential, gateway_core::account::ProviderAccount), ProviderAdminError>
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
            DeepSeekCredential::parse(loaded.credential.expose_to_provider())?,
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
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::BadGateway));
        }
        let bytes = transport::bounded_body(response, 4 * 1024 * 1024)
            .await
            .map_err(|_| unavailable())?;
        Ok((
            serde_json::from_slice(&bytes)
                .map_err(|_| ProviderAdminError::new(ProviderAdminErrorKind::BadGateway))?,
            account,
        ))
    }
}

#[async_trait]
impl ProviderAdmin for DeepSeekProvider {
    fn provider_kind(&self) -> &ProviderKind {
        &self.kind
    }
    fn account_capabilities(&self, _: &ProviderAccountId, _: &str) -> ProviderAccountCapabilities {
        ProviderAccountCapabilities {
            quota: true,
            quota_refresh: true,
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
        self.connection_test_operation_for_protocol(model, input, Some("openai"))
            .await
    }
    async fn connection_test_operation_for_protocol(
        &self,
        model: &UpstreamModelId,
        input: &str,
        protocol: Option<&str>,
    ) -> Result<Operation, ProviderAdminError> {
        let protocol = protocol.unwrap_or("openai");
        let value = match protocol {
            "openai" => {
                json!({"model":model.as_str(), "input":input, "stream":true, "max_output_tokens":256})
            }
            "openai-chat" => {
                json!({"model":model.as_str(), "messages":[{"role":"user","content":input}], "stream":true, "max_tokens":256, "stream_options":{"include_usage":true}})
            }
            "anthropic" => {
                json!({"model":model.as_str(), "messages":[{"role":"user","content":input}], "stream":true, "max_tokens":256})
            }
            _ => return Err(unsupported()),
        };
        let body = value.as_object().cloned().ok_or_else(invalid)?;
        Ok(Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(protocol, body).map_err(|_| invalid())?,
        )))
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
            let credential = DeepSeekCredential::parse(object)?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("DeepSeek")
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
        let credential = DeepSeekCredential::parse(&merged)?;
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
        let (value, observed_at) = if request.refresh {
            let (value, account) = self.admin_get(&request.account_id, "/user/balance").await?;
            let available = value
                .get("is_available")
                .and_then(Value::as_bool)
                .ok_or_else(unavailable)?;
            parse_balances(&value)?;
            let now = std::time::SystemTime::now();
            let quota = gateway_core::account::QuotaObservation {
                account_id: account.id().clone(),
                expected_revision: account.revision(),
                quota: document(value.clone())?.into_provider_data(),
                plan_type: None,
                observed_at: now,
                state: if available {
                    gateway_core::account::QuotaState::allowed(now)
                } else {
                    gateway_core::account::QuotaState::exhausted(
                        gateway_core::account::QuotaEvidence::UsageLimitReached,
                        now,
                        None,
                    )
                },
            };
            let outcome = self
                .ports
                .accounts()
                .compare_and_swap_quota(quota)
                .await
                .map_err(|_| unavailable())?;
            if outcome == gateway_core::account::QuotaWriteOutcome::Conflict {
                return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict));
            }
            (value, chrono::DateTime::<Utc>::from(now))
        } else {
            let observations = self
                .ports
                .accounts()
                .get_quotas(&[request.account_id])
                .await
                .map_err(|_| unavailable())?;
            let Some(observation) = observations.into_iter().next() else {
                return Ok(ProviderQuota {
                    plan_type: None,
                    observed_at: None,
                    refresh_token_expires_at: None,
                    windows: Vec::new(),
                    credits: None,
                    balances: Vec::new(),
                    limit_reached: false,
                    provider_data: None,
                });
            };
            (
                Value::Object(observation.quota.expose_to_provider().clone()),
                chrono::DateTime::<Utc>::from(observation.observed_at),
            )
        };
        let is_available = value
            .get("is_available")
            .and_then(Value::as_bool)
            .ok_or_else(unavailable)?;
        let balances = parse_balances(&value)?;
        Ok(ProviderQuota {
            plan_type: None,
            observed_at: Some(observed_at),
            refresh_token_expires_at: None,
            windows: Vec::new(),
            credits: None,
            balances,
            limit_reached: !is_available,
            provider_data: Some(document(json!({"is_available":is_available}))?),
        })
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
            let (value, loaded) = self.admin_get(account, "/models").await?;
            if loaded.revision() != current.revision() {
                return Err(ProviderAdminError::new(ProviderAdminErrorKind::Conflict));
            }
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
        let entries = value
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(unavailable)?;
        let models = entries
            .iter()
            .map(|entry| {
                let id = entry
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(unavailable)?;
                Ok(ProviderModel {
                    id: UpstreamModelId::new(id).map_err(|_| unavailable())?,
                    name: id.to_owned(),
                })
            })
            .collect::<Result<Vec<_>, ProviderAdminError>>()?;
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
            let credential = DeepSeekCredential::parse(
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

fn parse_balances(value: &Value) -> Result<Vec<ProviderMonetaryBalance>, ProviderAdminError> {
    let balances = value
        .get("balance_infos")
        .and_then(Value::as_array)
        .ok_or_else(unavailable)?
        .iter()
        .map(|entry| {
            Ok(ProviderMonetaryBalance {
                currency: entry
                    .get("currency")
                    .and_then(Value::as_str)
                    .ok_or_else(unavailable)?
                    .to_owned(),
                total_balance: entry
                    .get("total_balance")
                    .and_then(Value::as_str)
                    .ok_or_else(unavailable)?
                    .to_owned(),
                granted_balance: entry
                    .get("granted_balance")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                topped_up_balance: entry
                    .get("topped_up_balance")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect::<Result<Vec<_>, ProviderAdminError>>()?;

    Ok(balances)
}
