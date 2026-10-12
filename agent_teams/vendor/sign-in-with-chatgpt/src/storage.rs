//! 加密凭据、原子迁移与进程间目录锁，锁内操作不会暴露秘密到公共会话
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use crate::{CancellationToken, ChatGPTError, CredentialEncryption, errors::cancelled};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};
use uuid::Uuid;

pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
pub(crate) fn saved_at() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn error(code: &str) -> ChatGPTError {
    let (message, retryable) = match code {
        "storage_invalid" => (
            "Saved ChatGPT credentials could not be read. Restore the credential file before signing in.",
            false,
        ),
        "storage_unsafe" => (
            "Saved ChatGPT credentials must be a private, owner-only file.",
            false,
        ),
        "storage_lock_lost" => (
            "The credential storage lock was interrupted. Close other copies of the app and reconnect.",
            false,
        ),
        "storage_encryption_unavailable" => (
            "Secure credential storage is unavailable. Unlock your OS credential store and try again. Saved credentials have been preserved.",
            true,
        ),
        "storage_encrypted_invalid" => (
            "The encrypted credential file is damaged. Restore the file before signing in. Saved credentials have been preserved.",
            false,
        ),
        "storage_provider_mismatch" => (
            "Use the credential encryption provider that created this file. Saved credentials have been preserved.",
            false,
        ),
        "storage_decryption_failed" => (
            "Saved credentials could not be decrypted. Restore access to your OS credential store or restore the credential file. Saved credentials have been preserved.",
            false,
        ),
        "storage_encryption_failed" => (
            "ChatGPT credentials could not be encrypted. Check your OS credential store and try again. Saved credentials have been preserved.",
            true,
        ),
        "storage_too_large" => (
            "The encrypted credential file exceeds the supported size. Saved credentials have been preserved.",
            false,
        ),
        "host_identity_invalid" => (
            "The saved runtime identity is invalid. Restore chatgpt-host.json before signing in.",
            false,
        ),
        "storage_busy" => (
            "Another app process is updating this ChatGPT connection. Try again shortly.",
            true,
        ),
        _ => (
            "ChatGPT credentials could not be saved securely. Check the app's storage permissions.",
            false,
        ),
    };
    ChatGPTError::new(code, message, retryable)
}

pub(crate) fn identifier(s: &str, max: usize, alphabet: &[u8]) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || alphabet.contains(&c))
}
pub(crate) fn format_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && (s.as_bytes()[0].is_ascii_lowercase() || s.as_bytes()[0].is_ascii_digit())
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
}
fn strings(v: &Value) -> bool {
    v.as_array().is_some_and(|a| a.iter().all(Value::is_string))
}
fn optional(v: &Value, key: &str, predicate: impl Fn(&Value) -> bool) -> bool {
    v.get(key).is_none_or(predicate)
}
fn credentials(v: &Value, refresh: bool) -> bool {
    v.is_object()
        && v["accessToken"].as_str().is_some_and(|s| !s.is_empty())
        && v["expiresAt"].as_f64().is_some_and(f64::is_finite)
        && optional(v, "refreshToken", Value::is_string)
        && (!refresh || v["refreshToken"].as_str().is_some_and(|s| !s.is_empty()))
}
pub(crate) fn validate_record(v: &Value) -> Result<(), ChatGPTError> {
    let valid = v.is_object()
        && v["version"] == 1
        && v["clientId"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s != "dynamic_agent_client")
        && v["status"]
            .as_str()
            .is_some_and(|s| ["connected", "disconnected", "reauth_required"].contains(&s))
        && strings(&v["scopes"])
        && v["savedAt"].is_string()
        && optional(v, "subject", Value::is_string)
        && optional(v, "identity", |i| {
            i.is_object()
                && optional(i, "name", Value::is_string)
                && optional(i, "email", Value::is_string)
        })
        && optional(v, "credentials", |c| credentials(c, false))
        && optional(v, "profileIdToken", |t| {
            t.as_str().is_some_and(|s| !s.is_empty())
        })
        && optional(v, "pendingRefresh", |p| {
            p.is_object()
                && v["status"] == "connected"
                && v.get("credentials").is_some()
                && optional(p, "idToken", |v| v.as_str().is_some_and(|s| !s.is_empty()))
                && p["receivedAt"]
                    .as_i64()
                    .is_some_and(|n| n > 0 && n <= now() + 5000 && n <= 9_007_199_254_740_991)
                && strings(&p["scopes"])
                && credentials(&p["credentials"], true)
                && optional(&p["credentials"], "earliestRefreshAt", |e| {
                    e.is_string() || e.is_number()
                })
        });
    if valid {
        Ok(())
    } else {
        Err(error("storage_invalid"))
    }
}
pub(crate) fn validate_state(v: &Value) -> Result<(), ChatGPTError> {
    let invalid = || error("storage_invalid");
    if v["version"] != 2 {
        return Err(invalid());
    }
    let profiles = v["profiles"].as_array().ok_or_else(invalid)?;
    let pending = v["pendingRegistrations"].as_array().ok_or_else(invalid)?;
    let mut ids = HashSet::new();
    let mut labels = HashSet::new();
    for entry in profiles.iter().chain(pending) {
        let id = entry["id"].as_str().ok_or_else(invalid)?;
        let label = entry["label"].as_str().ok_or_else(invalid)?;
        let client = entry["clientId"].as_str().ok_or_else(invalid)?;
        if !identifier(id, 100, b"-")
            || label.trim().is_empty()
            || label.encode_utf16().count() > 120
            || label.chars().any(|c| c < ' ')
            || !identifier(client, 200, b"_-")
            || client == "dynamic_agent_client"
            || !entry["savedAt"].is_string()
            || !ids.insert(id)
            || !labels.insert(label)
        {
            return Err(invalid());
        }
    }
    for p in profiles {
        validate_record(p)?;
        if !optional(p, "requiresNewRegistration", Value::is_boolean) {
            return Err(invalid());
        }
    }
    if let Some(active) = v.get("activeProfileId")
        && (!active.is_string() || !profiles.iter().any(|p| p["id"] == *active))
    {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn empty_state() -> Value {
    json!({"version":2,"profiles":[],"pendingRegistrations":[]})
}

pub(crate) struct Store {
    directory: PathBuf,
    encryption: Arc<dyn CredentialEncryption>,
}

pub(crate) struct LockedStore<'a> {
    store: &'a Store,
    path: PathBuf,
    lost: Arc<AtomicBool>,
    heartbeat: tokio::task::JoinHandle<()>,
    identity: LockIdentity,
    released: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LockIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(not(unix))]
    created: SystemTime,
}
fn lock_identity(path: &Path) -> std::io::Result<LockIdentity> {
    let info = fs::symlink_metadata(path)?;
    if !info.is_dir() || info.file_type().is_symlink() {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(LockIdentity {
            device: info.dev(),
            inode: info.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(LockIdentity {
            created: info.created()?,
        })
    }
}
fn open_lock_directory(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x02000000);
    }
    options.open(path)
}
impl Drop for LockedStore<'_> {
    fn drop(&mut self) {
        self.heartbeat.abort();
        // 不删除后来进程接管的锁
        if !self.released
            && !self.lost.load(Ordering::SeqCst)
            && lock_identity(&self.path).ok() == Some(self.identity)
        {
            let _ = fs::remove_dir(&self.path);
        }
    }
}
impl Store {
    pub(crate) fn new(
        directory: PathBuf,
        encryption: Arc<dyn CredentialEncryption>,
    ) -> Result<Self, ChatGPTError> {
        if !format_id(encryption.id()) {
            return Err(ChatGPTError::new(
                "invalid_config",
                "Provide an OS-backed credential encryption provider in the local process.",
                false,
            ));
        }
        Ok(Self {
            directory,
            encryption,
        })
    }
    pub(crate) async fn lock(
        &self,
        signal: &CancellationToken,
    ) -> Result<LockedStore<'_>, ChatGPTError> {
        fs::create_dir_all(&self.directory).map_err(|_| error("storage_error"))?;
        let info = fs::symlink_metadata(&self.directory).map_err(|_| error("storage_error"))?;
        if !info.is_dir() || info.file_type().is_symlink() {
            return Err(error("storage_unsafe"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if info.uid() != unsafe { libc::getuid() } {
                return Err(error("storage_unsafe"));
            }
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))
                .map_err(|_| error("storage_error"))?;
        }
        let path = self.directory.join(".chatgpt-auth.lock");
        let started = tokio::time::Instant::now();
        loop {
            if signal.is_cancelled() {
                return Err(cancelled());
            }
            match fs::create_dir(&path) {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let meta = fs::symlink_metadata(&path).map_err(|_| error("storage_error"))?;
                    if !meta.is_dir() || meta.file_type().is_symlink() {
                        return Err(error("storage_unsafe"));
                    }
                    if meta
                        .modified()
                        .ok()
                        .and_then(|m| m.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(120))
                    {
                        // 原子改名隔离失效锁，避免按路径删除另一个进程刚获得的锁
                        let stale = self
                            .directory
                            .join(format!(".chatgpt-stale.{}", Uuid::new_v4()));
                        if fs::rename(&path, &stale).is_ok() {
                            let _ = fs::remove_dir(stale);
                        }
                        continue;
                    }
                    if started.elapsed() > Duration::from_secs(45) {
                        return Err(error("storage_busy"));
                    }
                    tokio::select! { _=signal.cancelled()=>return Err(cancelled()), _=tokio::time::sleep(Duration::from_millis(100))=>{} }
                }
                Err(_) => return Err(error("storage_error")),
            }
        }
        let identity = lock_identity(&path).map_err(|_| error("storage_error"))?;
        let lost = Arc::new(AtomicBool::new(false));
        let heartbeat_path = path.clone();
        let compromised = lost.clone();
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(20)).await;
                let ok = lock_identity(&heartbeat_path).ok() == Some(identity)
                    && open_lock_directory(&heartbeat_path)
                        .and_then(|f| f.set_modified(SystemTime::now()))
                        .is_ok();
                if !ok {
                    compromised.store(true, Ordering::SeqCst);
                    break;
                }
            }
        });
        Ok(LockedStore {
            store: self,
            path,
            lost,
            heartbeat,
            identity,
            released: false,
        })
    }
}
impl LockedStore<'_> {
    pub(crate) fn finish(mut self) -> Result<(), ChatGPTError> {
        self.assert_locked()?;
        self.heartbeat.abort();
        fs::remove_dir(&self.path).map_err(|_| error("storage_lock_lost"))?;
        self.released = true;
        Ok(())
    }
    fn assert_locked(&self) -> Result<(), ChatGPTError> {
        if self.lost.load(Ordering::SeqCst) || lock_identity(&self.path).ok() != Some(self.identity)
        {
            Err(error("storage_lock_lost"))
        } else {
            Ok(())
        }
    }
    async fn require_encryption(&self) -> Result<(), ChatGPTError> {
        if self.store.encryption.is_available().await {
            Ok(())
        } else {
            Err(error("storage_encryption_unavailable"))
        }
    }
    fn read_file(&self, name: &str) -> Result<Option<Value>, ChatGPTError> {
        self.assert_locked()?;
        let path = self.store.directory.join(name);
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(error("storage_invalid"));
            }
        }
        let mut file = match options.open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(error("storage_invalid")),
        };
        let info = file.metadata().map_err(|_| error("storage_invalid"))?;
        if !info.is_file() || info.len() > 2 * 1024 * 1024 {
            return Err(error("storage_unsafe"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if info.mode() & 0o077 != 0 || info.uid() != unsafe { libc::getuid() } {
                return Err(error("storage_unsafe"));
            }
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| error("storage_invalid"))?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(error("storage_unsafe"));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| error("storage_invalid"))
    }
    fn write_file(&self, name: &str, value: &Value) -> Result<(), ChatGPTError> {
        self.assert_locked()?;
        let bytes = serde_json::to_vec(value).map_err(|_| error("storage_error"))?;
        if bytes.len() > 2 * 1024 * 1024 {
            return Err(error("storage_too_large"));
        }
        let temp = self
            .store
            .directory
            .join(format!(".chatgpt-auth.{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp).map_err(|_| error("storage_error"))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| error("storage_error"))?;
            self.assert_locked()?;
            fs::rename(&temp, self.store.directory.join(name))
                .map_err(|_| error("storage_error"))?;
            #[cfg(unix)]
            File::open(&self.store.directory)
                .and_then(|f| f.sync_all())
                .map_err(|_| error("storage_error"))?;
            Ok(())
        })();
        let _ = fs::remove_file(temp);
        result
    }
    pub(crate) async fn read(&self) -> Result<Value, ChatGPTError> {
        self.assert_locked()?;
        self.require_encryption().await?;
        let Some(value) = self.read_file("chatgpt-auth.json")? else {
            return Ok(empty_state());
        };
        if value["version"] == 3 {
            let provider = value["provider"]
                .as_str()
                .ok_or_else(|| error("storage_encrypted_invalid"))?;
            let encoded = value["ciphertext"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| error("storage_encrypted_invalid"))?;
            let bytes = STANDARD
                .decode(encoded)
                .map_err(|_| error("storage_encrypted_invalid"))?;
            if STANDARD.encode(&bytes) != encoded {
                return Err(error("storage_encrypted_invalid"));
            }
            if provider != self.store.encryption.id() {
                return Err(error("storage_provider_mismatch"));
            }
            let text = self
                .store
                .encryption
                .decrypt(&bytes)
                .await
                .map_err(|_| error("storage_decryption_failed"))?;
            let state: Value =
                serde_json::from_str(&text).map_err(|_| error("storage_decryption_failed"))?;
            validate_state(&state).map_err(|_| error("storage_decryption_failed"))?;
            self.assert_locked()?;
            return Ok(state);
        }
        let migrated = if value["version"] == 1 {
            validate_record(&value)?;
            let id = Uuid::new_v4().to_string();
            let mut profile = value;
            profile["id"] = id.clone().into();
            profile["label"] = "Connection 1".into();
            profile["requiresNewRegistration"] = true.into();
            json!({"version":2,"activeProfileId":id,"profiles":[profile],"pendingRegistrations":[]})
        } else if value["version"] == 2 {
            validate_state(&value)?;
            value
        } else {
            return Err(error("storage_encrypted_invalid"));
        };
        self.write(&migrated).await?;
        Ok(migrated)
    }
    pub(crate) async fn write(&self, value: &Value) -> Result<(), ChatGPTError> {
        self.assert_locked()?;
        self.require_encryption().await?;
        validate_state(value)?;
        let text = serde_json::to_string(value).map_err(|_| error("storage_encryption_failed"))?;
        let bytes = self
            .store
            .encryption
            .encrypt(&text)
            .await
            .map_err(|_| error("storage_encryption_failed"))?;
        if bytes.is_empty() {
            return Err(error("storage_encryption_failed"));
        }
        self.write_file("chatgpt-auth.json",&json!({"version":3,"provider":self.store.encryption.id(),"ciphertext":STANDARD.encode(bytes)}))
    }
    pub(crate) async fn host_id(&self) -> Result<String, ChatGPTError> {
        self.assert_locked()?;
        self.require_encryption().await?;
        let value = self.read_file("chatgpt-host.json").map_err(|e| {
            if e.code == "storage_invalid" {
                error("host_identity_invalid")
            } else {
                e
            }
        })?;
        let Some(value) = value else {
            let id = format!("urn:uuid:{}", Uuid::new_v4());
            self.write_file("chatgpt-host.json", &json!({"version":1,"id":id}))?;
            return Ok(id);
        };
        let id = value["id"]
            .as_str()
            .ok_or_else(|| error("host_identity_invalid"))?;
        let valid = id
            .strip_prefix("urn:uuid:")
            .and_then(|s| Uuid::parse_str(s).ok())
            .is_some_and(|u| u.get_version_num() == 4 && format!("urn:uuid:{u}") == id);
        if value["version"] != 1 || !valid {
            return Err(error("host_identity_invalid"));
        }
        Ok(id.into())
    }
}

pub(crate) fn default_directory(app_id: &str) -> Result<PathBuf, ChatGPTError> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .ok_or_else(|| error("storage_error"))?;
    Ok(Path::new(&home).join(".config").join(app_id))
}
