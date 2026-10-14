//! 使用共享调度策略和 revision-fenced 租约为三种协议选择同一账号池

use crate::{
    DeepSeekCredential,
    provider::{DeepSeekProvider, not_sent},
};
use gateway_core::{
    account::{
        AccountCandidate, AccountEligibilityPolicy, AccountHistorySnapshot,
        AccountSelectionContext, AccountSelector, ProviderAccount,
    },
    concurrency::{CapacityWait, ConcurrencyWaitQueue},
    engine::{AttemptContext, policy::AccountPolicyError},
    error::{ProviderError, ProviderErrorKind},
    provider_ports::{
        ProviderConcurrencyPool, ProviderLeaseAcquisition, ProviderLeaseGuard,
        ProviderLeaseRequest, ProviderSchedulingLeaseRequest,
    },
    routing::ProviderCandidate,
};
use std::{sync::Arc, time::SystemTime};

pub(crate) struct Selected {
    pub account: ProviderAccount,
    pub credential: DeepSeekCredential,
    pub _lease: Box<dyn ProviderLeaseGuard>,
}
impl Selected {
    pub fn snapshot(&self) -> AccountHistorySnapshot {
        AccountHistorySnapshot::from_account(&self.account)
    }
}

impl DeepSeekProvider {
    pub(crate) async fn select(
        &self,
        candidate: &ProviderCandidate,
        protocol: &str,
        context: &AttemptContext,
    ) -> Result<Selected, ProviderError> {
        let policy = context.account_selection_policy();
        let diagnostic = context.is_diagnostic_required_account();
        let mut waiting = CapacityWait::new(
            &self.waiting,
            policy.queue_policy(),
            context.deadline().at(),
            context.concurrency_wait_budget(),
        );
        let model = candidate
            .upstream_model()
            .ok_or_else(|| not_sent(ProviderErrorKind::InvalidRequest))?;
        'refresh: loop {
            let mut accounts = self
                .ports
                .accounts()
                .list_for_provider(&self.kind)
                .await
                .map_err(|_| not_sent(ProviderErrorKind::ProviderInfrastructureUnavailable))?;
            if diagnostic
                && let Some(required) = context.required_account()
                && !accounts.iter().any(|a| a.id() == required)
                && let Some(account) = self
                    .ports
                    .accounts()
                    .get_account(required)
                    .await
                    .map_err(|_| not_sent(ProviderErrorKind::ProviderInfrastructureUnavailable))?
                && account.provider() == &self.kind
            {
                accounts.push(account);
            }
            accounts.retain(|a| {
                !context.excluded_accounts().contains(a.id())
                    && context.required_account().is_none_or(|id| id == a.id())
                    && (diagnostic
                        || candidate
                            .account_scope()
                            .allows_model(a.id(), model.as_str()))
            });
            if !diagnostic && !accounts.is_empty() {
                // Key 权限只说明允许使用账号；还必须与该账号当前凭据的模型目录取交集
                // 复用管理面 revision-scoped 缓存，读取失败绝不回退到未验证账号
                let catalogs = futures::future::join_all(accounts.iter().map(|account| {
                    gateway_admin::ports::provider::ProviderAdmin::models(self, account.id(), false)
                }))
                .await;
                let expected = accounts.len();
                let mut observed = 0;
                accounts = accounts
                    .into_iter()
                    .zip(catalogs)
                    .filter_map(|(account, catalog)| {
                        let Ok(catalog) = catalog else {
                            return None;
                        };
                        observed += 1;
                        catalog
                            .models
                            .iter()
                            .any(|entry| &entry.id == model)
                            .then_some(account)
                    })
                    .collect();
                if accounts.is_empty() {
                    // 全部目录均已验证才能断言不支持；部分读取失败仍属于基础设施未知
                    return Err(not_sent(if observed == expected {
                        ProviderErrorKind::Unsupported
                    } else {
                        ProviderErrorKind::ProviderInfrastructureUnavailable
                    }));
                }
            }
            let ids = accounts.iter().map(|a| a.id().clone()).collect::<Vec<_>>();
            let state = self
                .ports
                .leases()
                .load_state(
                    context.client_api_key_ref(),
                    &self.kind,
                    &ids,
                    ProviderConcurrencyPool::Shared,
                )
                .await
                .map_err(|_| not_sent(ProviderErrorKind::ProviderInfrastructureUnavailable))?;
            let mut candidates = Vec::new();
            let mut materials = std::collections::BTreeMap::new();
            for account in accounts {
                if !diagnostic
                    && self
                        .ports
                        .cooldowns()
                        .read(account.id())
                        .await
                        .map_err(|_| {
                            not_sent(ProviderErrorKind::ProviderInfrastructureUnavailable)
                        })?
                        .is_some_and(|cooldown| cooldown.until() > SystemTime::now())
                {
                    continue;
                }
                let loaded = match self
                    .ports
                    .accounts()
                    .load_credential(account.id(), account.revision())
                    .await
                {
                    Ok(loaded) => loaded,
                    Err(error) if error.kind() == gateway_core::error::StoreErrorKind::Conflict => {
                        continue 'refresh;
                    }
                    Err(_) => {
                        return Err(not_sent(
                            ProviderErrorKind::ProviderInfrastructureUnavailable,
                        ));
                    }
                };
                let credential =
                    match DeepSeekCredential::parse(loaded.credential.expose_to_provider()) {
                        Ok(value) => value,
                        Err(_) if diagnostic => {
                            return Err(not_sent(ProviderErrorKind::Unauthorized));
                        }
                        Err(_) => continue,
                    };
                if !credential.protocols.contains(protocol) {
                    continue;
                }
                materials.insert(account.id().clone(), credential);
                let signals = state.signals().get(account.id()).cloned().ok_or_else(|| {
                    not_sent(ProviderErrorKind::ProviderInfrastructureUnavailable)
                })?;
                let health = self
                    .ports
                    .account_feedback()
                    .scheduling_signals(&self.kind, account.id());
                candidates.push(AccountCandidate {
                    account,
                    signals: signals.with_runtime_health(health.0, health.1),
                });
            }
            let mut selection_context = AccountSelectionContext {
                policy,
                now: SystemTime::now(),
                excluded_accounts: context.excluded_accounts().clone(),
                preferred_account: context.required_account().cloned(),
                preferred_account_overrides_weight: false,
                round_robin_cursor: state.round_robin_cursor(),
                eligibility: if diagnostic {
                    AccountEligibilityPolicy::BypassForDiagnostic
                } else {
                    AccountEligibilityPolicy::Enforce
                },
                account_scope: (!diagnostic).then(|| Arc::clone(candidate.account_scope())),
                reserved_concurrency: 0,
            };
            let wait_candidates = AccountSelector.wait_candidates(&candidates, &selection_context);
            for candidate in &candidates {
                if !waiting.can_try(candidate.account.id()) {
                    selection_context
                        .excluded_accounts
                        .insert(candidate.account.id().clone());
                }
            }
            let mut busy = false;
            loop {
                let selection = match context.request_policy_context() {
                    Some(policy) => match policy
                        .select_account(
                            context.attempt_index(),
                            &self.kind,
                            Some(model.as_str()),
                            &candidates,
                            &selection_context,
                        )
                        .await
                    {
                        Ok(selection) => selection,
                        Err(AccountPolicyError::StaleCandidate) => continue 'refresh,
                        Err(AccountPolicyError::Rejected) => {
                            return Err(not_sent(ProviderErrorKind::RequestPolicyDenied));
                        }
                        Err(AccountPolicyError::Fault) => {
                            return Err(not_sent(
                                ProviderErrorKind::ProviderInfrastructureUnavailable,
                            ));
                        }
                    },
                    None => AccountSelector.select(&candidates, &selection_context),
                };
                let Some(selection) = selection else {
                    break;
                };
                let account = selection.candidate().account.clone();
                let credential = materials.remove(account.id()).ok_or_else(|| {
                    not_sent(ProviderErrorKind::ProviderInfrastructureUnavailable)
                })?;
                let acquired = self
                    .ports
                    .leases()
                    .try_acquire(ProviderLeaseRequest::Scheduling(
                        ProviderSchedulingLeaseRequest::new(
                            self.kind.clone(),
                            account.id().clone(),
                            account.revision(),
                            account.effective_concurrency(policy.max_concurrent_per_account()),
                            policy.request_interval(),
                            context.deadline(),
                        )
                        .with_cancellation(context.cancellation().clone()),
                    ))
                    .await
                    .map_err(|_| not_sent(ProviderErrorKind::ProviderInfrastructureUnavailable))?;
                match acquired {
                    ProviderLeaseAcquisition::Acquired(lease) => {
                        return Ok(Selected {
                            account,
                            credential,
                            _lease: lease,
                        });
                    }
                    ProviderLeaseAcquisition::Busy { .. } => {
                        busy = true;
                        candidates.retain(|a| a.account.id() != account.id());
                    }
                }
            }
            if !diagnostic
                && (busy || !candidates.is_empty())
                && policy.queue_policy().max_waiting > 0
                && !wait_candidates.is_empty()
            {
                AccountSelector
                    .wait_for_capacity(
                        &mut waiting,
                        &wait_candidates,
                        &candidates,
                        &selection_context,
                    )
                    .await
                    .map_err(|_| not_sent(ProviderErrorKind::ConcurrencyQueueTimeout))?;
                continue;
            }
            return Err(not_sent(if busy || !wait_candidates.is_empty() {
                ProviderErrorKind::AccountCapacityUnavailable
            } else {
                ProviderErrorKind::NoEligibleAccount
            }));
        }
    }
}

pub(crate) fn waiting_queue() -> ConcurrencyWaitQueue<gateway_core::account::ProviderAccountId> {
    ConcurrencyWaitQueue::default()
}
