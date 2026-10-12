//! 多账号登录状态机与安全订阅，刷新任务在调用方取消后仍完成轮换持久化
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use crate::{
    CancellationToken, ChatGPTConfig, ChatGPTError, ChatGPTModel, LoginProfile, ResponseText,
    SHARING_SCOPE, SessionState, SessionStatus, SignInOptions, StreamResponseOptions,
    errors::{cancelled, requires_reauthentication},
    oauth::{OAuth, revocation_failed, unavailable},
    responses,
    storage::{Store, default_directory, empty_state, format_id, now, saved_at},
    transport::{ReqwestTransport, Transport},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone)]
pub struct ChatGPT {
    inner: Arc<Inner>,
}
type SessionListener = Arc<dyn Fn(SessionState) + Send + Sync>;

/// 丢弃句柄即取消订阅，回调只接收不含凭据的完整快照
pub struct Subscription {
    inner: Weak<Inner>,
    id: Uuid,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner
                .listeners
                .lock()
                .expect("listeners lock")
                .remove(&self.id);
        }
    }
}
struct Inner {
    config: ChatGPTConfig,
    store: Store,
    oauth: OAuth,
    transport: Arc<dyn Transport>,
    state: Mutex<Runtime>,
    subscribers: watch::Sender<SessionState>,
    listeners: Mutex<HashMap<Uuid, SessionListener>>,
}
struct Runtime {
    pending: Option<(Uuid, CancellationToken)>,
    changing: bool,
    epoch: u64,
    requests: CancellationToken,
}
enum Change {
    SignIn(Uuid),
    Profile,
}
struct ChangeGuard {
    inner: Arc<Inner>,
    change: Change,
}
impl Drop for ChangeGuard {
    fn drop(&mut self) {
        let mut runtime = self.inner.state.lock().expect("state lock");
        match self.change {
            Change::SignIn(id) => {
                if runtime
                    .pending
                    .as_ref()
                    .is_some_and(|(current, _)| *current == id)
                    && let Some((_, token)) = runtime.pending.take()
                {
                    token.cancel();
                }
            }
            Change::Profile => runtime.changing = false,
        }
    }
}
struct RequestGuard(CancellationToken);
impl Drop for RequestGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

fn selected(saved: &Value) -> Option<&Value> {
    saved["profiles"]
        .as_array()?
        .iter()
        .find(|p| p["id"] == saved["activeProfileId"])
}
fn status(profile: &Value) -> SessionStatus {
    match profile["status"].as_str() {
        Some("connected") => SessionStatus::Connected,
        Some("reauth_required") => SessionStatus::ReauthRequired,
        _ => SessionStatus::Disconnected,
    }
}
fn sharing(profile: &Value) -> bool {
    profile["status"] == "connected"
        && profile.get("credentials").is_some()
        && profile["scopes"]
            .as_array()
            .is_some_and(|a| a.iter().any(|s| s == SHARING_SCOPE))
}
fn safe_session(saved: &Value) -> SessionState {
    let Some(profile) = selected(saved) else {
        return SessionState::default();
    };
    SessionState {
        status: status(profile),
        sharing: sharing(profile),
        profile_id: profile["id"].as_str().map(str::to_owned),
        profile_label: profile["label"].as_str().map(str::to_owned),
        identity: profile
            .get("identity")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        error: profile.get("pendingRefresh").map(|_| unavailable()),
    }
}
fn without_credentials(profile: &Value, new_status: &str) -> Value {
    let mut result = profile.clone();
    let object = result.as_object_mut().expect("validated profile");
    for key in ["credentials", "profileIdToken", "pendingRefresh"] {
        object.remove(key);
    }
    result["status"] = new_status.into();
    result["scopes"] = json!([]);
    result["savedAt"] = saved_at().into();
    result
}
fn label(saved: &Value, requested: Option<&str>) -> Result<String, ChatGPTError> {
    let used = |s: &str| {
        saved["profiles"]
            .as_array()
            .expect("validated profiles")
            .iter()
            .chain(
                saved["pendingRegistrations"]
                    .as_array()
                    .expect("validated pending"),
            )
            .any(|p| p["label"] == s)
    };
    if let Some(requested) = requested {
        let requested = requested.trim();
        if requested.is_empty()
            || requested.encode_utf16().count() > 100
            || requested.chars().any(|c| c < ' ')
        {
            return Err(ChatGPTError::new(
                "invalid_config",
                "Use a connection label between 1 and 100 characters.",
                false,
            ));
        }
        if !used(requested) {
            return Ok(requested.into());
        }
        for n in 2.. {
            let label = format!("{requested} ({n})");
            if !used(&label) {
                return Ok(label);
            }
        }
    } else {
        for n in 1.. {
            let label = format!("Connection {n}");
            if !used(&label) {
                return Ok(label);
            }
        }
    }
    unreachable!()
}
fn replace_profile(saved: &mut Value, profile: &Value) {
    let current = saved["profiles"]
        .as_array_mut()
        .expect("validated profiles")
        .iter_mut()
        .find(|p| p["id"] == profile["id"])
        .expect("locked profile remains present");
    *current = profile.clone();
}
fn changed() -> ChatGPTError {
    ChatGPTError::new(
        "connection_changed",
        "The selected ChatGPT connection changed during sign-in. Try again.",
        false,
    )
}
fn profile_missing() -> ChatGPTError {
    ChatGPTError::new(
        "profile_not_found",
        "The saved ChatGPT profile could not be found.",
        false,
    )
}
fn busy() -> ChatGPTError {
    ChatGPTError::new(
        "connection_busy",
        "Finish the connection change before continuing.",
        false,
    )
}
fn assert_idle(runtime: &Runtime) -> Result<(), ChatGPTError> {
    if runtime.pending.is_some() || runtime.changing {
        Err(busy())
    } else {
        Ok(())
    }
}
fn cancel_requests(runtime: &mut Runtime) {
    runtime.epoch += 1;
    runtime.requests.cancel();
    runtime.requests = CancellationToken::new();
}

impl ChatGPT {
    pub fn new(config: ChatGPTConfig) -> Result<Self, ChatGPTError> {
        Self::with_transport(config, Arc::new(ReqwestTransport::new()?))
    }
    /// 注入传输仅用于测试或可信本地适配器，官方 URL 与协议不由配置覆盖
    pub fn with_transport(
        config: ChatGPTConfig,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, ChatGPTError> {
        if config.app_name.trim().is_empty()
            || config.app_name.encode_utf16().count() > 120
            || config.app_name.chars().any(|c| c < ' ')
        {
            return Err(ChatGPTError::new(
                "invalid_config",
                "Provide a short, human-readable app name.",
                false,
            ));
        }
        if !format_id(&config.app_id) {
            return Err(ChatGPTError::new(
                "invalid_config",
                "Use a stable app ID containing lowercase letters, digits, dots, underscores, or hyphens.",
                false,
            ));
        }
        if config
            .storage_dir
            .as_ref()
            .is_some_and(|p| !p.as_os_str().is_empty() && !p.is_absolute())
        {
            return Err(ChatGPTError::new(
                "invalid_config",
                "ChatGPT storageDir must be an absolute path.",
                false,
            ));
        }
        let directory = config
            .storage_dir
            .clone()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Ok)
            .unwrap_or_else(|| default_directory(&config.app_id))?;
        let store = Store::new(directory, config.credential_encryption.clone())?;
        let (subscribers, _) = watch::channel(SessionState::default());
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                store,
                oauth: OAuth::new(transport.clone()),
                transport,
                subscribers,
                listeners: Mutex::new(HashMap::new()),
                state: Mutex::new(Runtime {
                    pending: None,
                    changing: false,
                    epoch: 0,
                    requests: CancellationToken::new(),
                }),
            }),
        })
    }
    fn publish(&self, state: SessionState) -> SessionState {
        self.inner.subscribers.send_replace(state.clone());
        let listeners: Vec<_> = self
            .inner
            .listeners
            .lock()
            .expect("listeners lock")
            .values()
            .cloned()
            .collect();
        for listener in listeners {
            // 订阅者不能打断凭据持久化或使状态锁中毒
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(state.clone())));
        }
        state
    }
    pub fn subscribe(
        &self,
        listener: impl Fn(SessionState) + Send + Sync + 'static,
    ) -> Subscription {
        let id = Uuid::new_v4();
        let listener: SessionListener = Arc::new(listener);
        self.inner
            .listeners
            .lock()
            .expect("listeners lock")
            .insert(id, listener.clone());
        let snapshot = self.inner.subscribers.borrow().clone();
        listener(snapshot);
        Subscription {
            inner: Arc::downgrade(&self.inner),
            id,
        }
    }
    async fn read(&self) -> Result<Value, ChatGPTError> {
        let lock = self.inner.store.lock(&CancellationToken::new()).await?;
        let saved = lock.read().await?;
        lock.finish()?;
        Ok(saved)
    }
    pub async fn get_session(&self) -> SessionState {
        {
            let runtime = self.inner.state.lock().expect("state lock");
            if runtime.pending.is_some() || runtime.changing {
                return self.inner.subscribers.borrow().clone();
            }
        }
        match self.read().await {
            Ok(saved) => {
                let mut next = safe_session(&saved);
                let current = self.inner.subscribers.borrow().clone();
                if current.profile_id == next.profile_id && current.error.is_some() {
                    next.error = current.error;
                }
                self.publish(next)
            }
            Err(error) => self.publish(SessionState {
                status: SessionStatus::ReauthRequired,
                error: Some(error),
                ..SessionState::default()
            }),
        }
    }
    pub async fn list_profiles(&self) -> Result<Vec<LoginProfile>, ChatGPTError> {
        let saved = self.read().await?;
        let mut profiles = Vec::new();
        for p in saved["profiles"].as_array().expect("validated profiles") {
            profiles.push(LoginProfile {
                id: p["id"].as_str().expect("validated id").into(),
                label: p["label"].as_str().expect("validated label").into(),
                status: status(p),
                sharing: sharing(p),
                identity: p
                    .get("identity")
                    .and_then(|v| serde_json::from_value(v.clone()).ok()),
                requires_new_registration: p["requiresNewRegistration"].as_bool().filter(|v| *v),
                pending: None,
            });
        }
        for p in saved["pendingRegistrations"]
            .as_array()
            .expect("validated pending")
        {
            profiles.push(LoginProfile {
                id: p["id"].as_str().expect("validated id").into(),
                label: p["label"].as_str().expect("validated label").into(),
                status: SessionStatus::Disconnected,
                sharing: false,
                identity: None,
                requires_new_registration: None,
                pending: Some(true),
            });
        }
        Ok(profiles)
    }
    pub fn cancel_sign_in(&self) {
        if let Some((_, token)) = &self.inner.state.lock().expect("state lock").pending {
            token.cancel();
        }
    }
    fn assert_current(&self, epoch: u64, signal: &CancellationToken) -> Result<(), ChatGPTError> {
        if signal.is_cancelled() || self.inner.state.lock().expect("state lock").epoch != epoch {
            Err(cancelled())
        } else {
            Ok(())
        }
    }
    pub async fn sign_in(&self, options: SignInOptions) -> Result<SessionState, ChatGPTError> {
        let (epoch, signal, _guard) = {
            let mut runtime = self.inner.state.lock().expect("state lock");
            assert_idle(&runtime)?;
            if options.new_profile && options.profile_id.is_some() {
                return Err(ChatGPTError::new(
                    "invalid_config",
                    "Choose either a new profile or a saved profile.",
                    false,
                ));
            }
            let id = Uuid::new_v4();
            let signal = CancellationToken::new();
            runtime.pending = Some((id, signal.clone()));
            cancel_requests(&mut runtime);
            (
                runtime.epoch,
                signal,
                ChangeGuard {
                    inner: self.inner.clone(),
                    change: Change::SignIn(id),
                },
            )
        };
        let mut connecting = self.inner.subscribers.borrow().clone();
        connecting.status = SessionStatus::Connecting;
        connecting.error = None;
        self.publish(connecting);
        let result = tokio::select! {
            biased;
            _=options.signal.cancelled()=> {signal.cancel();Err(cancelled())},
            _=signal.cancelled()=>Err(cancelled()),
            result=tokio::time::timeout(Duration::from_secs(600),self.sign_in_inner(&options,epoch,&signal))=>result.unwrap_or_else(|_| {signal.cancel();Err(cancelled())}),
        };
        if let Err(error) = &result
            && self.inner.state.lock().expect("state lock").epoch == epoch
        {
            let saved = self.read().await.unwrap_or_else(|_| empty_state());
            let mut state = safe_session(&saved);
            if error.code != "cancelled" {
                state.error = Some(error.clone());
            }
            self.publish(state);
        }
        result
    }
    async fn sign_in_inner(
        &self,
        options: &SignInOptions,
        epoch: u64,
        signal: &CancellationToken,
    ) -> Result<SessionState, ChatGPTError> {
        let (id, requested_label, profile, mut previous, host_id, active) = {
            let lock = self.inner.store.lock(signal).await?;
            let saved = lock.read().await?;
            let id = if !options.new_profile {
                options
                    .profile_id
                    .as_deref()
                    .or_else(|| saved["activeProfileId"].as_str())
                    .map(str::to_owned)
            } else {
                None
            }
            .unwrap_or_else(|| Uuid::new_v4().to_string());
            let profile = saved["profiles"]
                .as_array()
                .expect("validated profiles")
                .iter()
                .find(|p| p["id"] == id)
                .cloned();
            let pending = saved["pendingRegistrations"]
                .as_array()
                .expect("validated pending")
                .iter()
                .find(|p| p["id"] == id);
            if options.profile_id.is_some() && profile.is_none() && pending.is_none() {
                return Err(profile_missing());
            }
            if profile
                .as_ref()
                .is_some_and(|p| p["requiresNewRegistration"] == true)
            {
                return Err(ChatGPTError::new(
                    "registration_upgrade_required",
                    "This connection was registered with the older localhost callback. Add an account to register the new 127.0.0.1 callback. Your existing connection is preserved.",
                    false,
                ));
            }
            let requested_label = profile
                .as_ref()
                .or(pending)
                .and_then(|p| p["label"].as_str())
                .map(str::to_owned)
                .map(Ok)
                .unwrap_or_else(|| label(&saved, options.label.as_deref()))?;
            let previous=profile.clone().or_else(||pending.map(|p|json!({"version":1,"clientId":p["clientId"],"status":"disconnected","scopes":[],"savedAt":p["savedAt"]})));
            let host_id = lock.host_id().await?;
            lock.finish()?;
            (
                id,
                requested_label,
                profile,
                previous,
                host_id,
                saved.get("activeProfileId").cloned(),
            )
        };
        let mut issued_client = previous
            .as_ref()
            .and_then(|p| p["clientId"].as_str())
            .map(str::to_owned);
        let mut record = None;
        for attempt in 0..2 {
            let result=self.inner.oauth.authorize(&self.inner.config,previous.as_ref(),&host_id,signal,options.reconsent,|client_id| {
                issued_client=Some(client_id.clone());
                let id=&id; let profile=&profile; let requested_label=&requested_label;
                async move {
                    let lock=self.inner.store.lock(signal).await?; self.assert_current(epoch,signal)?; let mut saved=lock.read().await?;
                    let existing=saved["profiles"].as_array().expect("validated profiles").iter().find(|p|p["id"]==*id);
                    if let Some(existing)=existing {
                        if profile.as_ref().is_none_or(|p|existing["clientId"]!=client_id || existing["savedAt"]!=p["savedAt"] || existing["credentials"]["accessToken"]!=p["credentials"]["accessToken"]) { return Err(changed()); }
                        return lock.finish();
                    }
                    if profile.is_some() { return Err(changed()); }
                    let pending=saved["pendingRegistrations"].as_array().expect("validated pending").iter().find(|p|p["id"]==*id);
                    if pending.is_some_and(|p|p["clientId"]!=client_id) { return Err(changed()); }
                    if pending.is_none() {
                        let label=label(&saved,Some(requested_label))?;
                        saved["pendingRegistrations"].as_array_mut().expect("validated pending").push(json!({"id":id,"label":label,"clientId":client_id,"savedAt":saved_at()}));
                        self.assert_current(epoch,signal)?; lock.write(&saved).await?;
                    }
                    lock.finish()
                }
            }).await;
            match result {
                Ok(value) => {
                    record = Some(value);
                    break;
                }
                Err(e) if e.code == "invalid_grant" && attempt == 0 && issued_client.is_some() => {
                    if previous.is_none() {
                        previous = Some(
                            json!({"version":1,"clientId":issued_client,"status":"disconnected","scopes":[],"savedAt":saved_at()}),
                        );
                    }
                }
                Err(e) => return Err(e),
            }
        }
        let mut record = record.ok_or_else(|| {
            ChatGPTError::new(
                "sign_in_failed",
                "ChatGPT sign-in could not be completed.",
                false,
            )
        })?;
        let lock = self.inner.store.lock(signal).await?;
        self.assert_current(epoch, signal)?;
        let mut latest = lock.read().await?;
        let current = latest["profiles"]
            .as_array()
            .expect("validated profiles")
            .iter()
            .find(|p| p["id"] == id);
        if latest.get("activeProfileId").cloned() != active
            || profile.as_ref().is_some_and(|p| {
                current.is_none_or(|c| {
                    c["savedAt"] != p["savedAt"]
                        || c["credentials"]["accessToken"] != p["credentials"]["accessToken"]
                })
            })
        {
            return Err(changed());
        }
        let pending = latest["pendingRegistrations"]
            .as_array()
            .expect("validated pending")
            .iter()
            .find(|p| p["id"] == id);
        if pending.is_some_and(|p| p["clientId"] != record["clientId"]) {
            return Err(changed());
        }
        record["id"] = id.clone().into();
        record["label"] = current
            .or(pending)
            .map(|p| p["label"].clone())
            .unwrap_or(requested_label.into());
        latest["profiles"]
            .as_array_mut()
            .expect("validated profiles")
            .retain(|p| p["id"] != id);
        latest["profiles"]
            .as_array_mut()
            .expect("validated profiles")
            .push(record);
        latest["pendingRegistrations"]
            .as_array_mut()
            .expect("validated pending")
            .retain(|p| p["id"] != id);
        latest["activeProfileId"] = id.into();
        self.assert_current(epoch, signal)?;
        lock.write(&latest).await?;
        lock.finish()?;
        Ok(self.publish(safe_session(&latest)))
    }
    pub async fn select_profile(&self, id: &str) -> Result<SessionState, ChatGPTError> {
        let _guard = {
            let mut runtime = self.inner.state.lock().expect("state lock");
            assert_idle(&runtime)?;
            runtime.changing = true;
            cancel_requests(&mut runtime);
            ChangeGuard {
                inner: self.inner.clone(),
                change: Change::Profile,
            }
        };
        let lock = self.inner.store.lock(&CancellationToken::new()).await?;
        let mut saved = lock.read().await?;
        if !saved["profiles"]
            .as_array()
            .expect("validated profiles")
            .iter()
            .any(|p| p["id"] == id)
        {
            return Err(profile_missing());
        }
        saved["activeProfileId"] = id.into();
        lock.write(&saved).await?;
        lock.finish()?;
        Ok(self.publish(safe_session(&saved)))
    }
    pub async fn disconnect(&self) -> Result<(), ChatGPTError> {
        let _guard = {
            let mut runtime = self.inner.state.lock().expect("state lock");
            if runtime.changing {
                return Err(busy());
            }
            runtime.changing = true;
            cancel_requests(&mut runtime);
            if let Some((_, token)) = &runtime.pending {
                token.cancel();
            }
            ChangeGuard {
                inner: self.inner.clone(),
                change: Change::Profile,
            }
        };
        let lock = self.inner.store.lock(&CancellationToken::new()).await?;
        let mut saved = lock.read().await?;
        let mut failed = false;
        if let Some(profile) = selected(&saved).cloned() {
            if profile.get("credentials").is_some() {
                failed = self.inner.oauth.revoke(&profile).await.is_err();
            }
            let invalid = without_credentials(&profile, "disconnected");
            for p in saved["profiles"]
                .as_array_mut()
                .expect("validated profiles")
            {
                if p["id"] == profile["id"] {
                    *p = invalid.clone();
                }
            }
            lock.write(&saved).await?;
        }
        lock.finish()?;
        let mut state = safe_session(&saved);
        if failed {
            state.error = Some(revocation_failed());
        }
        self.publish(state);
        if failed {
            Err(revocation_failed())
        } else {
            Ok(())
        }
    }
    async fn credentials(
        &self,
        signal: &CancellationToken,
    ) -> Result<(String, SessionState), ChatGPTError> {
        let lock = self.inner.store.lock(signal).await?;
        let mut saved = lock.read().await?;
        let mut profile = selected(&saved)
            .cloned()
            .filter(|p| p["status"] == "connected")
            .ok_or_else(|| {
                ChatGPTError::new(
                    "sign_in_required",
                    "Sign in with ChatGPT to continue.",
                    false,
                )
            })?;
        let check = |profile: &Value| -> Result<(), ChatGPTError> {
            if !profile["scopes"]
                .as_array()
                .expect("validated scopes")
                .iter()
                .any(|s| s == SHARING_SCOPE)
            {
                return Err(ChatGPTError::new(
                    "sharing_not_enabled",
                    "You are signed in, but token sharing is disabled. Enable token sharing in your account settings to continue.",
                    false,
                ));
            }
            if profile.get("credentials").is_none() {
                return Err(ChatGPTError::new(
                    "sign_in_required",
                    "Your ChatGPT connection has no usable credentials. Sign in again.",
                    false,
                ));
            }
            Ok(())
        };
        check(&profile)?;
        if profile.get("pendingRefresh").is_some()
            || profile["credentials"]["expiresAt"]
                .as_f64()
                .expect("validated expiry")
                <= now() as f64 + 60_000.0
        {
            let earliest = &profile["credentials"]["earliestRefreshAt"];
            let earliest = earliest
                .as_f64()
                .map(|n| n * 1000.0)
                .or_else(|| {
                    earliest
                        .as_str()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.timestamp_millis() as f64)
                })
                .unwrap_or(0.0);
            if profile.get("pendingRefresh").is_some()
                || earliest <= now() as f64
                || !earliest.is_finite()
            {
                if signal.is_cancelled() {
                    return Err(cancelled());
                }
                // 脱离调用方取消的任务会持锁完成 token rotation 与 checkpoint
                let refresh_signal = CancellationToken::new();
                let operation = self
                    .inner
                    .oauth
                    .refresh(&profile, &refresh_signal, |rotation| {
                        let mut checkpoint = profile.clone();
                        checkpoint["pendingRefresh"] = rotation;
                        checkpoint["savedAt"] = saved_at().into();
                        replace_profile(&mut saved, &checkpoint);
                        let value = saved.clone();
                        let lock = &lock;
                        async move { lock.write(&value).await }
                    });
                // 远程调用各自有界，不能用外层超时丢弃已收到但尚在加密的 successor
                let refreshed = operation.await;
                match refreshed {
                    Ok(refreshed) => {
                        profile = refreshed;
                        replace_profile(&mut saved, &profile);
                        lock.write(&saved).await?;
                    }
                    Err(error) => {
                        if requires_reauthentication(&error)
                            || [
                                "invalid_id_token",
                                "invalid_token_response",
                                "account_mismatch",
                            ]
                            .contains(&error.code.as_str())
                        {
                            let current = selected(&saved).expect("selected profile");
                            let invalid = without_credentials(current, "reauth_required");
                            replace_profile(&mut saved, &invalid);
                            lock.write(&saved).await?;
                        }
                        return Err(error);
                    }
                }
            } else if profile["credentials"]["expiresAt"]
                .as_f64()
                .expect("validated expiry")
                <= now() as f64
            {
                return Err(ChatGPTError::new(
                    "refresh_not_ready",
                    "Your ChatGPT connection cannot refresh yet. Try again shortly.",
                    true,
                ));
            }
        }
        if profile["credentials"]["expiresAt"]
            .as_f64()
            .expect("validated expiry")
            <= now() as f64
        {
            return Err(ChatGPTError::new(
                "refresh_not_ready",
                "Your ChatGPT connection was recovered. Try the request again to renew it.",
                true,
            ));
        }
        check(&profile)?;
        lock.finish()?;
        Ok((
            profile["credentials"]["accessToken"]
                .as_str()
                .expect("validated access")
                .into(),
            safe_session(&saved),
        ))
    }
    async fn authenticated<T, F, Fut>(
        &self,
        caller: &CancellationToken,
        operation: F,
    ) -> Result<T, ChatGPTError>
    where
        F: FnOnce(String, CancellationToken) -> Fut,
        Fut: Future<Output = Result<T, ChatGPTError>>,
    {
        let (epoch, signal) = {
            let runtime = self.inner.state.lock().expect("state lock");
            assert_idle(&runtime)?;
            (runtime.epoch, runtime.requests.child_token())
        };
        let _guard = RequestGuard(signal.clone());
        let client = self.clone();
        let task_signal = signal.clone();
        let credentials = tokio::spawn(async move { client.credentials(&task_signal).await });
        let perform = async {
            let (access, state) = credentials.await.map_err(|_| {
                ChatGPTError::new(
                    "connection_error",
                    "The connection could not be completed. Try again.",
                    true,
                )
            })??;
            self.assert_current(epoch, &signal)?;
            self.publish(state);
            let result = operation(access, signal.clone()).await?;
            self.assert_current(epoch, &signal)?;
            Ok(result)
        };
        let result = tokio::select! {biased;_=caller.cancelled()=>Err(cancelled()),_=signal.cancelled()=>Err(cancelled()),result=perform=>result};
        if let Err(error) = &result
            && error.code != "cancelled"
            && self.inner.state.lock().expect("state lock").epoch == epoch
        {
            let mut state = self
                .read()
                .await
                .map(|saved| safe_session(&saved))
                .unwrap_or_else(|_| self.inner.subscribers.borrow().clone());
            state.error = Some(error.clone());
            self.publish(state);
        }
        result
    }
    pub async fn list_models(
        &self,
        signal: CancellationToken,
    ) -> Result<Vec<ChatGPTModel>, ChatGPTError> {
        self.authenticated(&signal, |access, signal| async move {
            responses::list_models(self.inner.transport.as_ref(), &access, &signal).await
        })
        .await
    }
    pub async fn stream_response(
        &self,
        options: StreamResponseOptions,
    ) -> Result<ResponseText, ChatGPTError> {
        if options.model.trim().is_empty() {
            return Err(ChatGPTError::new(
                "invalid_request",
                "Choose a model before starting a request.",
                false,
            ));
        }
        let caller = options.signal.clone();
        self.authenticated(&caller, |access, signal| async move {
            responses::stream_response(self.inner.transport.as_ref(), &access, &options, &signal)
                .await
        })
        .await
    }
}
