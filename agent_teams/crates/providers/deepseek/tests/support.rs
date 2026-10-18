//! DeepSeek 集成测试的内存账号、租约与无副作用存储端口
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use gateway_admin::model::{Revision, accounts::AccountRecord};
use gateway_core::{
    account::*,
    error::{StoreError, StoreErrorKind},
    provider_ports::*,
    routing::*,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};
#[derive(Clone)]
struct StoredAccount {
    account: ProviderAccount,
    credential: PlaintextCredential,
    quota: Option<QuotaObservation>,
}

#[derive(Default)]
pub struct MemoryProviderAccountStore {
    accounts: Mutex<BTreeMap<ProviderAccountId, StoredAccount>>,
    quota_reads: AtomicUsize,
    fail_provider_listing: AtomicBool,
    provider_listing_reads: AtomicUsize,
}

impl MemoryProviderAccountStore {
    pub fn provider_listing_reads(&self) -> usize {
        self.provider_listing_reads.load(Ordering::SeqCst)
    }
    pub fn account(&self, id: &ProviderAccountId) -> Option<ProviderAccount> {
        lock(&self.accounts)
            .get(id)
            .map(|stored| stored.account.clone())
    }
}

#[async_trait]
impl ProviderAccountStore for MemoryProviderAccountStore {
    async fn create_account(&self, account: NewProviderAccount) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        if accounts.contains_key(account.account.id()) {
            return Err(conflict());
        }
        accounts.insert(
            account.account.id().clone(),
            StoredAccount {
                account: account.account,
                credential: account.credential,
                quota: None,
            },
        );
        Ok(())
    }

    async fn get_account(
        &self,
        account: &ProviderAccountId,
    ) -> Result<Option<ProviderAccount>, StoreError> {
        Ok(self.account(account))
    }

    async fn list_accounts(&self) -> Result<Vec<ProviderAccount>, StoreError> {
        Ok(lock(&self.accounts)
            .values()
            .map(|stored| stored.account.clone())
            .collect())
    }

    async fn list_for_provider(
        &self,
        provider: &ProviderKind,
    ) -> Result<Vec<ProviderAccount>, StoreError> {
        self.provider_listing_reads.fetch_add(1, Ordering::SeqCst);
        if self.fail_provider_listing.load(Ordering::SeqCst) {
            return Err(invalid());
        }
        // 与 Postgres 实现的调度列表语义一致：停用账号不进入常规候选
        Ok(lock(&self.accounts)
            .values()
            .filter(|stored| stored.account.provider() == provider && stored.account.enabled())
            .map(|stored| stored.account.clone())
            .collect())
    }

    async fn list_refresh_candidates(
        &self,
        query: ProviderRefreshQuery,
    ) -> Result<Vec<LoadedCredential>, StoreError> {
        let mut candidates = lock(&self.accounts)
            .values()
            .filter(|stored| query.contains(&stored.account))
            .map(|stored| LoadedCredential {
                account: stored.account.clone(),
                credential: stored.credential.clone(),
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|loaded| {
            (
                loaded.account.access_token_expires_at(),
                loaded.account.id().clone(),
            )
        });
        candidates.truncate(query.limit().get() as usize);
        Ok(candidates)
    }

    async fn load_credential(
        &self,
        account: &ProviderAccountId,
        expected_revision: CredentialRevision,
    ) -> Result<LoadedCredential, StoreError> {
        let loaded = self.load_current_credential(account).await?;
        if loaded.account.revision() != expected_revision {
            return Err(conflict());
        }
        Ok(loaded)
    }

    async fn load_current_credential(
        &self,
        account: &ProviderAccountId,
    ) -> Result<LoadedCredential, StoreError> {
        let accounts = lock(&self.accounts);
        let stored = accounts.get(account).ok_or_else(invalid)?;
        Ok(LoadedCredential {
            account: stored.account.clone(),
            credential: stored.credential.clone(),
        })
    }

    async fn compare_and_swap_credential(
        &self,
        update: CredentialCasUpdate,
    ) -> Result<CredentialCasOutcome, StoreError> {
        let CredentialCasUpdateParts {
            account_id,
            expected_revision,
            profile,
            preserve_profile,
            credential,
            has_refresh_token,
            access_token_expires_at,
            next_refresh_at,
            account_state,
        } = update.into_parts();
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&account_id).ok_or_else(invalid)?;
        if stored.account.revision() != expected_revision {
            return Ok(CredentialCasOutcome::Conflict);
        }
        let next = expected_revision.next().map_err(|_| invalid())?;
        let credential_state = account_state
            .as_ref()
            .map_or(stored.account.credential_state(), |state| {
                state.credential_state
            });
        let last_error_reason = account_state.as_ref().map_or_else(
            || stored.account.last_error_reason(),
            |state| state.error_reason,
        );
        let last_error_message = account_state.as_ref().map_or_else(
            || stored.account.last_error_message().map(str::to_owned),
            |state| state.message.clone(),
        );
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: next,
                access_token_expires_at,
                credential_state,
                quota: stored.account.quota(),
                last_error_reason,
                last_error_message,
                enabled: stored.account.enabled(),
                has_refresh_token,
                next_refresh_at,
                name: if preserve_profile {
                    stored.account.name().to_owned()
                } else {
                    profile.name
                },
                email: if preserve_profile {
                    stored.account.email().map(str::to_owned)
                } else {
                    profile.email
                },
                plan_type: if preserve_profile {
                    stored.account.plan_type().map(str::to_owned)
                } else {
                    profile.plan_type
                },
            },
        );
        stored.credential = credential;
        stored.quota = None;
        Ok(CredentialCasOutcome::Updated(next))
    }

    async fn get_quotas(
        &self,
        accounts: &[ProviderAccountId],
    ) -> Result<Vec<QuotaObservation>, StoreError> {
        self.quota_reads.fetch_add(1, Ordering::SeqCst);
        let stored = lock(&self.accounts);
        Ok(accounts
            .iter()
            .filter_map(|account| stored.get(account)?.quota.clone())
            .collect())
    }

    async fn compare_and_swap_quota(
        &self,
        observation: QuotaObservation,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts
            .get_mut(&observation.account_id)
            .ok_or_else(invalid)?;
        if stored.account.revision() != observation.expected_revision {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        let quota = observation.state;
        stored.quota = Some(observation);
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement::preserving(&stored.account).with_quota(quota),
        );
        Ok(QuotaWriteOutcome::Updated)
    }

    async fn touch_quota_observation(
        &self,
        touch: QuotaObservationTouch,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&touch.account_id).ok_or_else(invalid)?;
        if stored.account.revision() != touch.expected_revision {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        let Some(quota) = &mut stored.quota else {
            return Ok(QuotaWriteOutcome::Conflict);
        };
        if quota.observed_at > touch.observed_at {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        quota.observed_at = touch.observed_at;
        Ok(QuotaWriteOutcome::Updated)
    }

    async fn apply_quota_access(
        &self,
        change: QuotaAccessChange,
    ) -> Result<QuotaWriteOutcome, StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&change.account_id).ok_or_else(invalid)?;
        if stored.account.revision() != change.expected_revision {
            return Ok(QuotaWriteOutcome::Conflict);
        }
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement::preserving(&stored.account).with_quota(change.state),
        );
        if let Some(observation) = &mut stored.quota {
            observation.state = change.state;
        }
        Ok(QuotaWriteOutcome::Updated)
    }

    async fn apply_state_change(&self, change: AccountStateChange) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&change.account_id).ok_or_else(invalid)?;
        if stored.account.revision() != change.expected_revision {
            return Err(conflict());
        }
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: stored.account.revision(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                credential_state: change.credential_state,
                quota: stored.account.quota(),
                last_error_reason: change.error_reason,
                last_error_message: change.message.clone(),
                enabled: stored.account.enabled(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                name: stored.account.name().to_owned(),
                email: stored.account.email().map(str::to_owned),
                plan_type: stored.account.plan_type().map(str::to_owned),
            },
        );
        Ok(())
    }

    async fn update_account(&self, update: ProviderAccountUpdate) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(&update.account_id).ok_or_else(invalid)?;
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: stored.account.revision(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                credential_state: stored.account.credential_state(),
                quota: stored.account.quota(),
                last_error_reason: stored.account.last_error_reason(),
                last_error_message: stored.account.last_error_message().map(str::to_owned),
                enabled: stored.account.enabled(),
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                name: update.name,
                email: update.email,
                plan_type: update.plan_type,
            },
        );
        Ok(())
    }

    async fn set_enabled(
        &self,
        account: &ProviderAccountId,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let mut accounts = lock(&self.accounts);
        let stored = accounts.get_mut(account).ok_or_else(invalid)?;
        stored.account = rebuild_account(
            &stored.account,
            AccountReplacement {
                revision: stored.account.revision(),
                access_token_expires_at: stored.account.access_token_expires_at(),
                credential_state: stored.account.credential_state(),
                quota: stored.account.quota(),
                last_error_reason: stored.account.last_error_reason(),
                last_error_message: stored.account.last_error_message().map(str::to_owned),
                enabled,
                has_refresh_token: stored.account.has_refresh_token(),
                next_refresh_at: stored.account.next_refresh_at(),
                name: stored.account.name().to_owned(),
                email: stored.account.email().map(str::to_owned),
                plan_type: stored.account.plan_type().map(str::to_owned),
            },
        );
        Ok(())
    }

    async fn delete_account(&self, account: &ProviderAccountId) -> Result<(), StoreError> {
        lock(&self.accounts).remove(account).ok_or_else(invalid)?;
        Ok(())
    }
}

struct AccountReplacement {
    revision: CredentialRevision,
    access_token_expires_at: Option<SystemTime>,
    credential_state: CredentialState,
    quota: QuotaState,
    last_error_reason: Option<AccountErrorReason>,
    last_error_message: Option<String>,
    enabled: bool,
    has_refresh_token: bool,
    next_refresh_at: Option<SystemTime>,
    name: String,
    email: Option<String>,
    plan_type: Option<String>,
}

impl AccountReplacement {
    fn preserving(account: &ProviderAccount) -> Self {
        Self {
            revision: account.revision(),
            access_token_expires_at: account.access_token_expires_at(),
            credential_state: account.credential_state(),
            quota: account.quota(),
            last_error_reason: account.last_error_reason(),
            last_error_message: account.last_error_message().map(str::to_owned),
            enabled: account.enabled(),
            has_refresh_token: account.has_refresh_token(),
            next_refresh_at: account.next_refresh_at(),
            name: account.name().to_owned(),
            email: account.email().map(str::to_owned),
            plan_type: account.plan_type().map(str::to_owned),
        }
    }

    fn with_quota(mut self, quota: QuotaState) -> Self {
        self.quota = quota;
        self
    }
}

fn rebuild_account(previous: &ProviderAccount, replacement: AccountReplacement) -> ProviderAccount {
    ProviderAccount::new(
        previous.id().clone(),
        previous.provider().clone(),
        replacement.name,
        previous.upstream_user_id().map(str::to_owned),
        previous.authentication_kind().to_owned(),
        replacement.revision,
        replacement.access_token_expires_at,
    )
    .with_profile(
        replacement.email,
        previous.upstream_account_id().map(str::to_owned),
        replacement.plan_type,
    )
    .with_account_facts(
        replacement.enabled,
        replacement.credential_state,
        replacement.quota,
        replacement.last_error_reason,
        replacement.last_error_message,
    )
    .with_scheduling(previous.concurrency_limit(), previous.weight())
    .with_refresh_schedule(replacement.has_refresh_token, replacement.next_refresh_at)
    .with_outbound_proxy(previous.outbound_proxy().cloned())
}

#[derive(Default)]
struct TestLeases {
    in_flight: Arc<AtomicUsize>,
}
struct TestLease(Arc<AtomicUsize>);
impl Drop for TestLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl ProviderLeasePort for TestLeases {
    fn load_state<'a>(
        &'a self,
        _: &'a gateway_core::policy::ClientApiKeyId,
        _: &'a gateway_core::routing::ProviderKind,
        accounts: &'a [ProviderAccountId],
        _pool: gateway_core::provider_ports::ProviderConcurrencyPool,
    ) -> BoxFuture<
        'a,
        Result<gateway_core::provider_ports::ProviderSchedulingState, ProviderStoreError>,
    > {
        Box::pin(async move {
            let signals = accounts
                .iter()
                .cloned()
                .map(|account| {
                    (
                        account,
                        AccountRuntimeSignals {
                            in_flight: self.in_flight.load(Ordering::SeqCst) as u32,
                            last_started_at: None,
                            quota_reset_at: None,
                            quota_remaining_rank: None,
                            cooldown: None,
                            failure_rate_basis_points: None,
                            first_output_latency_ms: None,
                        },
                    )
                })
                .collect();
            Ok(gateway_core::provider_ports::ProviderSchedulingState::new(
                signals, 0,
            ))
        })
    }

    fn try_acquire(
        &self,
        request: ProviderLeaseRequest,
    ) -> BoxFuture<'_, Result<ProviderLeaseAcquisition, ProviderStoreError>> {
        Box::pin(async move {
            let ProviderLeaseRequest::Scheduling(request) = request else {
                panic!("scheduling only");
            };
            assert_eq!(request.concurrency_pool(), ProviderConcurrencyPool::Shared);
            let limit = request
                .max_concurrent()
                .limit()
                .map(|v| v.get() as usize)
                .unwrap_or(usize::MAX);
            if self
                .in_flight
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                    (n < limit).then_some(n + 1)
                })
                .is_err()
            {
                return Ok(ProviderLeaseAcquisition::Busy { retry_after: None });
            }
            Ok(ProviderLeaseAcquisition::Acquired(Box::new(TestLease(
                self.in_flight.clone(),
            ))))
        })
    }
}

struct TestArtifactProfiles;
impl ProviderArtifactProfileCachePort for TestArtifactProfiles {
    fn replace_if_newer(
        &self,
        _profile: ProviderArtifactProfile,
        _ttl: Duration,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(true) })
    }

    fn read<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _artifact_key: &'a str,
    ) -> BoxFuture<'a, Result<Option<ProviderArtifactProfile>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }
}

struct TestCredentialState;
impl ProviderCredentialStatePort for TestCredentialState {
    fn replace(
        &self,
        _state: ProviderCredentialState,
    ) -> BoxFuture<'_, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn read<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCredentialState>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn record_refresh_backoff<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _window: Duration,
    ) -> BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async { Ok(1) })
    }

    fn clear_refresh_backoff<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }
}

struct TestCooldown;
impl ProviderCooldownPort for TestCooldown {
    fn put_if_later(
        &self,
        _cooldown: ProviderCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<ProviderCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn put_scoped_if_later(
        &self,
        _cooldown: ProviderScopedCooldown,
    ) -> BoxFuture<'_, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn read_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
    ) -> BoxFuture<'a, Result<Option<ProviderScopedCooldown>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }

    fn clear_scoped<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _scope: &'a ProviderCooldownScope,
        _through_revision: CredentialRevision,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn clear_all<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }

    fn record_capacity_failure<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _window: Duration,
        _in_flight: u32,
    ) -> BoxFuture<'a, Result<u32, ProviderStoreError>> {
        Box::pin(async { Ok(0) })
    }

    fn clear_after_success<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
        _through_revision: gateway_core::account::CredentialRevision,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async { Ok(()) })
    }

    fn capacity_peak_in_flight<'a>(
        &'a self,
        _account_id: &'a ProviderAccountId,
    ) -> BoxFuture<'a, Result<Option<u32>, ProviderStoreError>> {
        Box::pin(async { Ok(None) })
    }
}

struct TestRuntimePolicy;
impl ProviderRuntimePolicyPort for TestRuntimePolicy {
    fn load_refresh_policy(
        &self,
    ) -> BoxFuture<'_, Result<ProviderRefreshPolicy, ProviderStoreError>> {
        Box::pin(async {
            ProviderRefreshPolicy::try_new(
                Duration::from_secs(300),
                NonZeroU32::new(4).expect("nonzero concurrency"),
            )
        })
    }
}

#[derive(Default)]
struct TestCatalogCache {
    values: Mutex<BTreeMap<String, OpaqueProviderData>>,
}
impl ProviderCatalogCachePort for TestCatalogCache {
    fn replace<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
        catalog: &'a OpaqueProviderData,
        _ttl: Duration,
    ) -> BoxFuture<'a, Result<(), ProviderStoreError>> {
        Box::pin(async move {
            self.values
                .lock()
                .expect("catalog cache")
                .insert(key.scope().as_str().to_owned(), catalog.clone());
            Ok(())
        })
    }

    fn read<'a>(
        &'a self,
        key: &'a ProviderCatalogCacheKey,
    ) -> BoxFuture<'a, Result<Option<OpaqueProviderData>, ProviderStoreError>> {
        Box::pin(async move {
            Ok(self
                .values
                .lock()
                .expect("catalog cache")
                .get(key.scope().as_str())
                .cloned())
        })
    }
}
#[derive(Default)]
struct TestOAuthPending {
    values: Mutex<BTreeMap<PendingKey, PendingValue>>,
}
type PendingKey = (String, String);
type PendingValue = (String, OpaqueProviderData, SystemTime, Option<String>);
impl OAuthPendingFlowPort for TestOAuthPending {
    fn put_if_absent(
        &self,
        flow: NewOAuthPendingFlow,
    ) -> BoxFuture<'_, Result<OAuthPendingPutOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                flow.provider_kind().as_str().to_owned(),
                flow.flow().expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            if values.contains_key(&key) {
                return Ok(OAuthPendingPutOutcome::AlreadyExists);
            }
            values.insert(
                key,
                (
                    flow.owner().expose_to_store().to_owned(),
                    flow.payload().clone(),
                    SystemTime::now() + flow.ttl(),
                    None,
                ),
            );
            Ok(OAuthPendingPutOutcome::Stored)
        })
    }

    fn claim_if_owner<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a OAuthPendingBinding,
        owner: &'a OAuthPendingBinding,
        claim: &'a OAuthPendingBinding,
        _claim_ttl: Duration,
    ) -> BoxFuture<'a, Result<OAuthPendingClaimOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, payload, expires_at, stored_claim)) = values.get_mut(&key)
            else {
                return Ok(OAuthPendingClaimOutcome::NotFound);
            };
            if *expires_at <= SystemTime::now() {
                values.remove(&key);
                return Ok(OAuthPendingClaimOutcome::NotFound);
            }
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingClaimOutcome::OwnerMismatch);
            }
            if stored_claim.is_some() {
                return Ok(OAuthPendingClaimOutcome::InProgress);
            }
            *stored_claim = Some(claim.expose_to_store().to_owned());
            Ok(OAuthPendingClaimOutcome::Claimed(payload.clone()))
        })
    }

    fn release_claim<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a OAuthPendingBinding,
        owner: &'a OAuthPendingBinding,
        claim: &'a OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingReleaseOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, _, _, stored_claim)) = values.get_mut(&key) else {
                return Ok(OAuthPendingReleaseOutcome::NotFound);
            };
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingReleaseOutcome::OwnerMismatch);
            }
            if stored_claim.as_deref() != Some(claim.expose_to_store()) {
                return Ok(OAuthPendingReleaseOutcome::ClaimMismatch);
            }
            *stored_claim = None;
            Ok(OAuthPendingReleaseOutcome::Released)
        })
    }

    fn consume_claim<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        flow: &'a OAuthPendingBinding,
        owner: &'a OAuthPendingBinding,
        claim: &'a OAuthPendingBinding,
    ) -> BoxFuture<'a, Result<OAuthPendingConsumeOutcome, ProviderStoreError>> {
        Box::pin(async move {
            let key = (
                provider_kind.as_str().to_owned(),
                flow.expose_to_store().to_owned(),
            );
            let mut values = self.values.lock().expect("OAuth pending");
            let Some((stored_owner, _, _, stored_claim)) = values.get(&key) else {
                return Ok(OAuthPendingConsumeOutcome::NotFound);
            };
            if stored_owner != owner.expose_to_store() {
                return Ok(OAuthPendingConsumeOutcome::OwnerMismatch);
            }
            if stored_claim.as_deref() != Some(claim.expose_to_store()) {
                return Ok(OAuthPendingConsumeOutcome::ClaimMismatch);
            }
            values.remove(&key);
            Ok(OAuthPendingConsumeOutcome::Consumed)
        })
    }
}
struct TestSessionAffinity;
impl ProviderSessionAffinityPort for TestSessionAffinity {
    fn load<'a>(
        &'a self,
        _: &'a ProviderKind,
        _: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<
        'a,
        Result<Option<gateway_core::provider_ports::ProviderSessionBinding>, ProviderStoreError>,
    > {
        Box::pin(async { Ok(None) })
    }
    fn compare_and_bind<'a>(
        &'a self,
        _: &'a ProviderKind,
        _: &'a ProviderSessionAffinityKey,
        _: Option<&'a gateway_core::provider_ports::ProviderSessionBinding>,
        account: &'a ProviderAccountId,
        _: Duration,
    ) -> futures::future::BoxFuture<
        'a,
        Result<Option<gateway_core::provider_ports::ProviderSessionBinding>, ProviderStoreError>,
    > {
        Box::pin(async move {
            gateway_core::provider_ports::ProviderSessionBinding::new(
                account.clone(),
                "00000000000000000000000000000000".to_owned(),
            )
            .map(Some)
        })
    }
    fn load_alias<'a>(
        &'a self,
        _: &'a ProviderKind,
        _: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<
        'a,
        Result<Option<gateway_core::provider_ports::ProviderSessionAlias>, ProviderStoreError>,
    > {
        Box::pin(async { Ok(None) })
    }
    fn bind_alias<'a>(
        &'a self,
        _: &'a ProviderKind,
        _: &'a ProviderSessionAffinityKey,
        _: &'a gateway_core::provider_ports::ProviderSessionAlias,
        _: Duration,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(true) })
    }
}
struct TestSessionExclusions;
impl ProviderSessionExclusionPort for TestSessionExclusions {
    fn load<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
    ) -> futures::future::BoxFuture<'a, Result<Option<ProviderSessionExclusions>, ProviderStoreError>>
    {
        Box::pin(async { Ok(None) })
    }

    fn record_failure<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
        _account_id: &'a ProviderAccountId,
        _ttl: Duration,
    ) -> futures::future::BoxFuture<'a, Result<ProviderSessionExclusions, ProviderStoreError>> {
        Box::pin(async {
            Ok(ProviderSessionExclusions::new(
                BTreeSet::new(),
                String::new(),
            ))
        })
    }

    fn clear<'a>(
        &'a self,
        _provider_kind: &'a ProviderKind,
        _key: &'a ProviderSessionAffinityKey,
        _expected_revision: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<bool, ProviderStoreError>> {
        Box::pin(async { Ok(false) })
    }
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn conflict() -> StoreError {
    StoreError::new(StoreErrorKind::Conflict)
}
fn invalid() -> StoreError {
    StoreError::new(StoreErrorKind::InvalidData)
}
pub fn account_record(account: &ProviderAccount) -> AccountRecord {
    let now = Utc::now();
    AccountRecord {
        notes: None,
        model_access: Default::default(),
        outbound_proxy: None,
        id: account.id().to_string(),
        provider_kind: account.provider().clone(),
        groups: Vec::new(),
        name: account.name().to_owned(),
        email: account.email().map(str::to_owned),
        upstream_user_id: account.upstream_user_id().map(str::to_owned),
        upstream_account_id: account.upstream_account_id().map(str::to_owned),
        plan_type: account.plan_type().map(str::to_owned),
        authentication_kind: account.authentication_kind().to_owned(),
        credential_revision: Revision::new(account.revision().get()).expect("revision"),
        has_refresh_token: account.has_refresh_token(),
        access_token_expires_at: account.access_token_expires_at().map(DateTime::<Utc>::from),
        next_refresh_at: account.next_refresh_at().map(DateTime::<Utc>::from),
        enabled: account.enabled(),
        concurrency_limit: account.concurrency_limit(),
        weight: account.weight(),
        credential_state: account.credential_state(),
        credential_observed_at: now,
        quota: account.quota(),
        last_error_reason: account.last_error_reason(),
        last_error_message: account.last_error_message().map(str::to_owned),
        created_at: now,
        updated_at: now,
    }
}
struct Diagnostics;
#[async_trait]
impl gateway_core::diagnostics::OperationalDiagnostics for Diagnostics {
    async fn record_failure(
        &self,
        _: gateway_core::diagnostics::OperationalFailure,
    ) -> Result<(), StoreError> {
        Ok(())
    }
}
pub fn ports(accounts: Arc<MemoryProviderAccountStore>) -> ProviderStorePorts {
    ProviderStorePorts::new(
        accounts,
        Arc::new(TestLeases::default()),
        Arc::new(TestSessionAffinity),
        Arc::new(TestSessionExclusions),
        Arc::new(TestCatalogCache::default()),
        Arc::new(TestArtifactProfiles),
        Arc::new(TestCredentialState),
        Arc::new(TestCooldown),
        Arc::new(TestRuntimePolicy),
        Arc::new(TestOAuthPending::default()),
        Arc::new(Diagnostics),
    )
}
pub async fn fixture(
    base: &str,
) -> (
    Arc<provider_deepseek::DeepSeekProvider>,
    Arc<MemoryProviderAccountStore>,
    ProviderAccountId,
) {
    fixture_with_protocols(base, &provider_deepseek::PROTOCOLS).await
}
pub async fn fixture_with_protocols(
    base: &str,
    protocols: &[&str],
) -> (
    Arc<provider_deepseek::DeepSeekProvider>,
    Arc<MemoryProviderAccountStore>,
    ProviderAccountId,
) {
    fixture_with_schedule(base, protocols, None).await
}
pub async fn fixture_with_schedule(
    base: &str,
    protocols: &[&str],
    pricing_schedule: Option<serde_json::Value>,
) -> (
    Arc<provider_deepseek::DeepSeekProvider>,
    Arc<MemoryProviderAccountStore>,
    ProviderAccountId,
) {
    let store = Arc::new(MemoryProviderAccountStore::default());
    let id = ProviderAccountId::new("acct_deepseek_test").unwrap();
    let account = ProviderAccount::new(
        id.clone(),
        ProviderKind::new("deepseek").unwrap(),
        "DeepSeek".to_owned(),
        None,
        "api_key".to_owned(),
        CredentialRevision::new(1).unwrap(),
        None,
    )
    .with_account_facts(
        true,
        CredentialState::Ready,
        QuotaState::unknown(),
        None,
        None,
    );
    store
        .create_account(NewProviderAccount {
            account,
            model_access: None,
            credential: PlaintextCredential::new(
                {
                    let mut value = serde_json::json!({"api_key":"synthetic-key","base_url":base,"protocols":protocols});
                    if let Some(schedule) = pricing_schedule { value["pricing_schedule"] = schedule; }
                    value.as_object().unwrap().clone()
                },
            ),
        })
        .await
        .unwrap();
    (
        Arc::new(provider_deepseek::DeepSeekProvider::new(ports(store.clone())).unwrap()),
        store,
        id,
    )
}
pub fn request(
    operation: gateway_core::operation::Operation,
) -> gateway_core::engine::provider::ProviderRequest {
    request_with_accounts(
        operation,
        &[ProviderAccountId::new("acct_deepseek_test").unwrap()],
    )
}
pub fn request_with_accounts(
    operation: gateway_core::operation::Operation,
    ids: &[ProviderAccountId],
) -> gateway_core::engine::provider::ProviderRequest {
    use gateway_core::{
        account::scope::{
            ClientRoutingScope, FrozenAccountScope, RuntimeAccount, RuntimeAccountDirectory,
        },
        operation::OperationKind,
    };
    let provider = ProviderKind::new("deepseek").unwrap();
    let model = ProviderModel::new(
        provider.clone(),
        UpstreamModelId::new("deepseek-flash").unwrap(),
        ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), None)
            .with_protocols(provider_deepseek::PROTOCOLS),
    );
    let scope = Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(
            ids.iter()
                .cloned()
                .map(|id| (id, RuntimeAccount::new(provider.clone(), BTreeSet::new())))
                .collect(),
        )),
        ClientRoutingScope::all_accounts(),
    ));
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(2, 0, "smart", Default::default(), None, None),
        vec![provider],
        vec![model],
        vec![],
    )
    .unwrap();
    let plan = snapshot
        .plan(
            &PublicModelId::new("deepseek-flash").unwrap(),
            &operation,
            scope,
            &RoutingContext::default(),
        )
        .unwrap();
    gateway_core::engine::provider::ProviderRequest::new(operation, plan.candidates()[0].clone())
}
pub fn context(
    cancellation: gateway_core::lifecycle::CancellationToken,
) -> gateway_core::engine::AttemptContext {
    context_with_pricing(cancellation, Default::default())
}
pub fn context_with_pricing(
    cancellation: gateway_core::lifecycle::CancellationToken,
    pricing: gateway_core::metering::PricingOverrides,
) -> gateway_core::engine::AttemptContext {
    context_with_pricing_and_transport(
        cancellation,
        pricing,
        gateway_core::engine::execution::ClientTransport::InternalProbe,
    )
}
pub fn context_with_transport(
    cancellation: gateway_core::lifecycle::CancellationToken,
    transport: gateway_core::engine::execution::ClientTransport,
) -> gateway_core::engine::AttemptContext {
    context_with_pricing_and_transport(cancellation, Default::default(), transport)
}
fn context_with_pricing_and_transport(
    cancellation: gateway_core::lifecycle::CancellationToken,
    pricing: gateway_core::metering::PricingOverrides,
    transport: gateway_core::engine::execution::ClientTransport,
) -> gateway_core::engine::AttemptContext {
    context_with_budget(cancellation, pricing, transport, Duration::from_secs(30))
}
pub fn context_with_budget(
    cancellation: gateway_core::lifecycle::CancellationToken,
    pricing: gateway_core::metering::PricingOverrides,
    transport: gateway_core::engine::execution::ClientTransport,
    budget: Duration,
) -> gateway_core::engine::AttemptContext {
    context_with_rotation_and_budget(
        cancellation,
        pricing,
        transport,
        budget,
        RotationStrategy::Smart,
    )
}
pub fn round_robin_context() -> gateway_core::engine::AttemptContext {
    context_with_rotation_and_budget(
        gateway_core::lifecycle::CancellationToken::new(),
        Default::default(),
        gateway_core::engine::execution::ClientTransport::HttpSse,
        Duration::from_secs(30),
        RotationStrategy::RoundRobin,
    )
}
fn context_with_rotation_and_budget(
    cancellation: gateway_core::lifecycle::CancellationToken,
    pricing: gateway_core::metering::PricingOverrides,
    transport: gateway_core::engine::execution::ClientTransport,
    budget: Duration,
    strategy: RotationStrategy,
) -> gateway_core::engine::AttemptContext {
    gateway_core::engine::AttemptContext::new(
        gateway_core::engine::RequestAttemptContext::new(
            gateway_core::engine::ModelRequestId::new("req_deepseek_test").unwrap(),
            gateway_core::policy::ClientApiKeyId::new("key_deepseek_test").unwrap(),
        )
        .with_pricing(Arc::new(pricing))
        .with_middleware(None, Arc::from([]), "/v1/responses".to_owned(), transport),
        NonZeroU32::MIN,
        SystemTime::now() + budget,
        AccountSelectionPolicy::new(strategy, NonZeroU32::new(2).unwrap(), Duration::ZERO),
        gateway_core::engine::AccountAttemptContext::new(BTreeSet::new(), None, None),
        None,
        cancellation,
    )
}

// 执行测试预置已观察到的目录，不把冷推理请求测试变成目录请求测试
pub async fn execution_fixture(
    base: &str,
) -> (
    Arc<provider_deepseek::DeepSeekProvider>,
    Arc<MemoryProviderAccountStore>,
    ProviderAccountId,
) {
    execution_fixture_with_schedule(base, None).await
}
pub async fn execution_fixture_with_schedule(
    base: &str,
    pricing_schedule: Option<serde_json::Value>,
) -> (
    Arc<provider_deepseek::DeepSeekProvider>,
    Arc<MemoryProviderAccountStore>,
    ProviderAccountId,
) {
    let (_, store, id) =
        fixture_with_schedule(base, &provider_deepseek::PROTOCOLS, pricing_schedule).await;
    let ports = ports(store.clone());
    ports
        .catalog_cache()
        .replace(
            &ProviderCatalogCacheKey::new(
                ProviderKind::new("deepseek").unwrap(),
                ProviderCatalogScope::new(format!("{}:1", id.as_str())).unwrap(),
            ),
            &OpaqueProviderData::new(
                serde_json::json!({"data":[{"id":"deepseek-flash"}]})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            Duration::from_secs(300),
        )
        .await
        .unwrap();
    (
        Arc::new(provider_deepseek::DeepSeekProvider::new(ports).unwrap()),
        store,
        id,
    )
}
