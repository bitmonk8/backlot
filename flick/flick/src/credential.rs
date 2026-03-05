use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use zeroize::Zeroizing;

use crate::ApiKind;
use crate::error::CredentialError;

const NONCE_LEN: usize = 12;
const PREFIX: &str = "enc3:";

/// Internal serialized form for the credentials file.
#[derive(Debug, Serialize, Deserialize)]
struct StoredProvider {
    key: String,
    api: ApiKind,
    base_url: String,
}

/// Returned by `get()` — contains decrypted key + provider metadata.
pub struct ProviderEntry {
    pub key: String,
    pub api: ApiKind,
    pub base_url: String,
}

/// Returned by `list()` — provider name + metadata (no key).
pub struct ProviderInfo {
    pub name: String,
    pub api: ApiKind,
    pub base_url: String,
}

/// Encrypted credential store at `~/.flick/`.
pub struct CredentialStore {
    dir: PathBuf,
}

impl CredentialStore {
    pub fn new() -> Result<Self, CredentialError> {
        let dir = flick_dir()?;
        Ok(Self { dir })
    }

    /// Constructor with explicit directory, for testing.
    pub const fn with_dir(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }

    /// Decrypt and return the provider entry (key + metadata) for the given provider name.
    pub async fn get(&self, provider: &str) -> Result<ProviderEntry, CredentialError> {
        let key = self.load_secret_key().await?;
        let creds = self.load_credentials_file().await?;
        let stored = creds
            .get(provider)
            .ok_or_else(|| CredentialError::NotFound(provider.to_string()))?;
        let decrypted_key = decrypt(&key, &stored.key, provider)?;
        Ok(ProviderEntry {
            key: decrypted_key,
            api: stored.api,
            base_url: stored.base_url.clone(),
        })
    }

    /// Return sorted provider info from the credentials file.
    pub async fn list(&self) -> Result<Vec<ProviderInfo>, CredentialError> {
        let creds = self.load_credentials_file().await?;
        Ok(creds
            .into_iter()
            .map(|(name, stored)| ProviderInfo {
                name,
                api: stored.api,
                base_url: stored.base_url,
            })
            .collect())
    }

    /// Encrypt and store a provider entry.
    pub async fn set(
        &self,
        provider: &str,
        api_key: &str,
        api: ApiKind,
        base_url: &str,
    ) -> Result<(), CredentialError> {
        let key = self.load_or_create_secret_key().await?;
        let encrypted = encrypt(&key, api_key, provider)?;

        let mut creds = self.load_credentials_file().await?;
        creds.insert(
            provider.to_string(),
            StoredProvider {
                key: encrypted,
                api,
                base_url: base_url.to_string(),
            },
        );
        self.write_credentials_file(&creds).await?;
        Ok(())
    }

    fn secret_key_path(&self) -> PathBuf {
        self.dir.join(".secret_key")
    }

    fn credentials_path(&self) -> PathBuf {
        self.dir.join("credentials")
    }

    async fn load_secret_key(&self) -> Result<Zeroizing<[u8; 32]>, CredentialError> {
        let path = self.secret_key_path();
        let hex_str = Zeroizing::new(match tokio::fs::read_to_string(&path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(CredentialError::NoSecretKey(path));
            }
            Err(e) => return Err(CredentialError::Io(e)),
        });
        let bytes = Zeroizing::new(
            hex::decode(hex_str.trim())
                .map_err(|_| CredentialError::InvalidFormat("secret key: invalid hex".into()))?,
        );
        let key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| CredentialError::InvalidFormat("secret key must be 32 bytes".into()))?;
        Ok(Zeroizing::new(key))
    }

    async fn load_or_create_secret_key(&self) -> Result<Zeroizing<[u8; 32]>, CredentialError> {
        match self.load_secret_key().await {
            Ok(key) => return Ok(key),
            Err(CredentialError::NoSecretKey(_)) => {} // create below
            Err(e) => return Err(e),
        }
        let mut key = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(&mut *key);
        tokio::fs::create_dir_all(&self.dir).await?;
        let path = self.secret_key_path();
        let hex_key = Zeroizing::new(hex::encode(*key));

        // Create file with restrictive permissions atomically to prevent
        // a window where the file exists with default (world-readable) permissions.
        #[cfg(unix)]
        {
            use tokio::io::AsyncWriteExt;
            let file_result = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .await;
            match file_result {
                Ok(mut file) => {
                    file.write_all(hex_key.as_bytes()).await?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    return self.load_secret_key().await;
                }
                Err(e) => return Err(CredentialError::Io(e)),
            }
        }
        #[cfg(windows)]
        {
            use tokio::io::AsyncWriteExt;
            // Write directly with create_new to prevent concurrent setup
            // from overwriting an existing key (which would destroy credentials
            // encrypted under it).
            let file_result = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .await;
            match file_result {
                Ok(mut file) => {
                    if let Err(e) = file.write_all(hex_key.as_bytes()).await {
                        drop(file);
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(CredentialError::Io(e));
                    }
                    drop(file);
                    if let Err(e) = restrict_windows_permissions(&path) {
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(e);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Another process created the key concurrently — use theirs.
                    return self.load_secret_key().await;
                }
                Err(e) => return Err(CredentialError::Io(e)),
            }
        }

        Ok(key)
    }

    async fn load_credentials_file(
        &self,
    ) -> Result<BTreeMap<String, StoredProvider>, CredentialError> {
        let path = self.credentials_path();
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(CredentialError::Io(e)),
        };
        toml::from_str::<BTreeMap<String, StoredProvider>>(&text)
            .map_err(|e| CredentialError::InvalidFormat(e.to_string()))
    }

    async fn write_credentials_file(
        &self,
        creds: &BTreeMap<String, StoredProvider>,
    ) -> Result<(), CredentialError> {
        let text = toml::to_string(creds)
            .map_err(|e| CredentialError::InvalidFormat(e.to_string()))?;
        let path = self.credentials_path();

        // Write to temp file, set permissions, then rename atomically to prevent
        // partial writes and permission races on the credentials file.
        let tmp_path = path.with_extension("tmp");
        tokio::fs::write(&tmp_path, &text).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600)).await?;
        }
        #[cfg(windows)]
        {
            if let Err(e) = restrict_windows_permissions(&tmp_path) {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(e);
            }
        }
        tokio::fs::rename(&tmp_path, &path).await?;
        Ok(())
    }
}

pub fn flick_dir() -> Result<PathBuf, CredentialError> {
    let home = home_dir().ok_or_else(|| {
        CredentialError::InvalidFormat("HOME/USERPROFILE not set".into())
    })?;
    Ok(home.join(".flick"))
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Remove inherited ACEs and grant only the current user full control.
/// Equivalent security outcome to Unix `0o600`.
///
/// Uses Win32 security APIs directly instead of shelling out to icacls.
/// Gets the current user's SID from the process token, builds a DACL
/// with a single ACE granting that SID full control, and applies it
/// with inheritance protection.
#[cfg(windows)]
#[allow(unsafe_code, clippy::too_many_lines)]
fn restrict_windows_permissions(path: &std::path::Path) -> Result<(), CredentialError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{
        CloseHandle, LocalFree, HANDLE, HLOCAL, ERROR_SUCCESS, WIN32_ERROR,
    };
    use windows::Win32::Security::Authorization::{
        SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, SE_FILE_OBJECT,
        SET_ACCESS, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        GetTokenInformation, ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE,
        OBJECT_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID,
        TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::PCWSTR;

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    struct AclGuard(*mut ACL);
    impl Drop for AclGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = LocalFree(Some(HLOCAL(self.0.cast())));
                }
            }
        }
    }

    fn win32_err(context: &str, code: WIN32_ERROR) -> CredentialError {
        CredentialError::InvalidFormat(format!("{context}: error code {}", code.0))
    }

    // Get current user's SID from process token
    let token = {
        let mut h = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut h) }
            .map_err(|e| CredentialError::InvalidFormat(format!("OpenProcessToken: {e}")))?;
        h
    };
    let _token_guard = HandleGuard(token);

    // Query TOKEN_USER size, then fill buffer
    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &raw mut needed) };

    // Validate that the probe returned a usable size
    if needed == 0 {
        return Err(CredentialError::InvalidFormat(
            "GetTokenInformation probe returned size 0".into(),
        ));
    }

    // u64-aligned buffer satisfies TOKEN_USER alignment requirements
    let align_len = (needed as usize).div_ceil(std::mem::size_of::<u64>());
    let mut aligned: Vec<u64> = vec![0u64; align_len];
    let buffer: &mut [u8] = unsafe {
        std::slice::from_raw_parts_mut(aligned.as_mut_ptr().cast(), needed as usize)
    };
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            needed,
            &raw mut needed,
        )
    }
    .map_err(|e| CredentialError::InvalidFormat(format!("GetTokenInformation: {e}")))?;

    // SAFETY: aligned is u64-aligned (>= TOKEN_USER alignment) and large enough
    // per GetTokenInformation.
    let user_sid: PSID = unsafe { (*aligned.as_ptr().cast::<TOKEN_USER>()).User.Sid };

    // Build DACL: single ACE granting current user FILE_ALL_ACCESS
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: 0x001F_01FF, // FILE_ALL_ACCESS
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: windows::core::PWSTR(user_sid.0.cast()),
            ..Default::default()
        },
    };

    let mut acl_ptr = std::ptr::null_mut::<ACL>();
    let result = unsafe { SetEntriesInAclW(Some(&[ea]), None, &raw mut acl_ptr) };
    if result != ERROR_SUCCESS {
        return Err(win32_err("SetEntriesInAclW", result));
    }
    let _acl_guard = AclGuard(acl_ptr);

    // Apply to file: set DACL with inheritance protection
    let path_wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let sec_info: OBJECT_SECURITY_INFORMATION =
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;

    let result = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(path_wide.as_ptr()),
            SE_FILE_OBJECT,
            sec_info,
            None,
            None,
            Some(acl_ptr),
            None,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(win32_err("SetNamedSecurityInfoW", result));
    }

    Ok(())
}

fn encrypt(key: &[u8; 32], plaintext: &str, provider: &str) -> Result<String, CredentialError> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, Payload { msg: plaintext.as_bytes(), aad: provider.as_bytes() })
        .map_err(|_| CredentialError::InvalidFormat("encryption failed".into()))?;

    let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);
    Ok(format!("{PREFIX}{}", hex::encode(combined)))
}

fn decrypt(key: &[u8; 32], value: &str, provider: &str) -> Result<String, CredentialError> {
    let hex_str = value
        .strip_prefix(PREFIX)
        .ok_or_else(|| CredentialError::InvalidFormat(format!("missing {PREFIX} prefix")))?;
    let combined =
        hex::decode(hex_str).map_err(|_| CredentialError::InvalidFormat("bad hex".into()))?;
    if combined.len() < NONCE_LEN + 16 {
        return Err(CredentialError::InvalidFormat("too short".into()));
    }
    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher = ChaCha20Poly1305::new(key.into());
    let plaintext = cipher
        .decrypt(nonce, Payload { msg: ciphertext, aad: provider.as_bytes() })
        .map_err(|_| CredentialError::DecryptionFailed(provider.to_string()))?;
    String::from_utf8(plaintext)
        .map_err(|_| CredentialError::DecryptionFailed(provider.to_string()))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let key = [42u8; 32];
        let plaintext = "sk-test-api-key-12345";
        let encrypted = encrypt(&key, plaintext, "test").expect("encryption should succeed");
        assert!(encrypted.starts_with(PREFIX));
        let decrypted = decrypt(&key, &encrypted, "test").expect("decryption should succeed");
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn decrypt_wrong_key_fails() {
        let key1 = [1u8; 32];
        let key2 = [2u8; 32];
        let encrypted = encrypt(&key1, "secret", "test").expect("encryption should succeed");
        let result = decrypt(&key2, &encrypted, "test");
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_bad_prefix_fails() {
        let key = [42u8; 32];
        let result = decrypt(&key, "notencrypted", "test");
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_too_short_fails() {
        let key = [42u8; 32];
        let result = decrypt(&key, "enc3:aabb", "test");
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_aad_mismatch_fails() {
        let key = [42u8; 32];
        let encrypted = encrypt(&key, "secret", "anthropic").expect("encrypt");
        let result = decrypt(&key, &encrypted, "openai");
        assert!(matches!(result, Err(CredentialError::DecryptionFailed(_))));
    }

    #[tokio::test]
    async fn list_returns_provider_info_after_set() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());

        store.set("openai", "key1", ApiKind::ChatCompletions, "https://api.openai.com").await.expect("set openai");
        store.set("anthropic", "key2", ApiKind::Messages, "https://api.anthropic.com").await.expect("set anthropic");

        let providers = store.list().await.expect("list");
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name, "anthropic");
        assert_eq!(providers[0].api, ApiKind::Messages);
        assert_eq!(providers[1].name, "openai");
        assert_eq!(providers[1].api, ApiKind::ChatCompletions);
    }

    #[tokio::test]
    async fn list_returns_empty_when_no_credentials() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());

        let providers = store.list().await.expect("list");
        assert!(providers.is_empty());
    }

    #[tokio::test]
    async fn list_returns_sorted_order() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());

        store.set("zebra", "k", ApiKind::ChatCompletions, "https://example.com").await.expect("set");
        store.set("alpha", "k", ApiKind::ChatCompletions, "https://example.com").await.expect("set");
        store.set("middle", "k", ApiKind::ChatCompletions, "https://example.com").await.expect("set");

        let providers = store.list().await.expect("list");
        let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "middle", "zebra"]);
    }

    #[tokio::test]
    async fn credential_store_set_then_get_round_trip() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());

        store
            .set("anthropic", "sk-ant-test-key-123", ApiKind::Messages, "https://api.anthropic.com")
            .await
            .expect("set should succeed");
        let entry = store.get("anthropic").await.expect("get should succeed");
        assert_eq!(entry.key, "sk-ant-test-key-123");
        assert_eq!(entry.api, ApiKind::Messages);
        assert_eq!(entry.base_url, "https://api.anthropic.com");
    }

    #[tokio::test]
    async fn credential_store_multiple_providers_independent() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());

        store.set("provider_a", "key_a", ApiKind::Messages, "https://a.com").await.expect("set a");
        store.set("provider_b", "key_b", ApiKind::ChatCompletions, "https://b.com").await.expect("set b");

        assert_eq!(store.get("provider_a").await.expect("get a").key, "key_a");
        assert_eq!(store.get("provider_b").await.expect("get b").key, "key_b");
    }

    #[tokio::test]
    async fn credential_store_overwrite_existing() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());

        store.set("openai", "old-key", ApiKind::ChatCompletions, "https://api.openai.com").await.expect("set old");
        store.set("openai", "new-key", ApiKind::ChatCompletions, "https://api.openai.com").await.expect("set new");

        assert_eq!(store.get("openai").await.expect("get").key, "new-key");
    }

    #[tokio::test]
    async fn credential_store_get_not_found() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());

        // Set something to create the secret key file
        store.set("existing", "key", ApiKind::Messages, "https://example.com").await.expect("set");
        let result = store.get("nonexistent").await;
        assert!(matches!(result, Err(CredentialError::NotFound(_))));
    }

    #[tokio::test]
    async fn corrupted_key_file_invalid_hex() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let key_path = dir.path().join(".secret_key");
        std::fs::write(&key_path, "not-valid-hex!@#$").expect("write key");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let result = store.get("test").await;
        assert!(matches!(result, Err(CredentialError::InvalidFormat(_))));
    }

    #[tokio::test]
    async fn corrupted_key_file_wrong_length() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let key_path = dir.path().join(".secret_key");
        std::fs::write(&key_path, "aabbccdd").expect("write key");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let result = store.get("test").await;
        assert!(matches!(result, Err(CredentialError::InvalidFormat(_))));
    }

    #[tokio::test]
    async fn corrupted_key_file_blocks_set() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let key_path = dir.path().join(".secret_key");
        std::fs::write(&key_path, "not-hex").expect("write key");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        let result = store.set("test", "key-value", ApiKind::Messages, "https://example.com").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn malformed_credentials_toml_propagates_error() {
        let dir = tempfile::tempdir().expect("create tempdir");
        // Create a valid key first
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        store.set("seed", "value", ApiKind::Messages, "https://example.com").await.expect("initial set");
        // Corrupt the credentials file
        let creds_path = dir.path().join("credentials");
        std::fs::write(&creds_path, "[[[invalid toml").expect("corrupt file");
        let result = store.set("test", "new-key", ApiKind::Messages, "https://example.com").await;
        assert!(matches!(result, Err(CredentialError::InvalidFormat(_))));
    }

    #[test]
    fn encrypt_decrypt_empty_string() {
        let key = [42u8; 32];
        let encrypted = encrypt(&key, "", "test").expect("encrypt empty");
        let decrypted = decrypt(&key, &encrypted, "test").expect("decrypt empty");
        assert_eq!(decrypted, "");
    }

    #[test]
    fn encrypt_decrypt_multibyte_utf8() {
        let key = [42u8; 32];
        let text = "こんにちは世界 🌍 émojis";
        let encrypted = encrypt(&key, text, "test").expect("encrypt utf8");
        let decrypted = decrypt(&key, &encrypted, "test").expect("decrypt utf8");
        assert_eq!(decrypted, text);
    }

    #[tokio::test]
    async fn credential_store_persists_across_instances() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().to_path_buf();

        let store1 = CredentialStore::with_dir(path.clone());
        store1.set("test", "persistent-key", ApiKind::Messages, "https://example.com").await.expect("set");

        let store2 = CredentialStore::with_dir(path);
        assert_eq!(store2.get("test").await.expect("get").key, "persistent-key");
    }

    #[tokio::test]
    async fn non_string_toml_value_returns_invalid_format() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        // Create a valid secret key first
        store.set("seed", "value", ApiKind::Messages, "https://example.com").await.expect("initial set");
        // Write credentials with integer value instead of string
        let creds_path = dir.path().join("credentials");
        std::fs::write(&creds_path, "provider = 42\n").expect("write");
        let result = store.get("provider").await;
        assert!(matches!(result, Err(CredentialError::InvalidFormat(_))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_file_permissions_are_600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());
        store.set("test_provider", "secret-key", ApiKind::Messages, "https://example.com").await.expect("set credential");

        let key_path = dir.path().join(".secret_key");
        let key_perms = std::fs::metadata(&key_path)
            .expect("read key metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(key_perms, 0o600, "secret key should be mode 0600, got {key_perms:o}");

        let creds_path = dir.path().join("credentials");
        let creds_perms = std::fs::metadata(&creds_path)
            .expect("read creds metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(creds_perms, 0o600, "credentials should be mode 0600, got {creds_perms:o}");
    }

    #[test]
    fn decrypt_valid_prefix_invalid_hex() {
        let key = [42u8; 32];
        let result = decrypt(&key, "enc3:not_hex_at_all!", "test");
        assert!(matches!(result, Err(CredentialError::InvalidFormat(_))));
    }

    #[test]
    #[allow(unsafe_code)]
    #[serial_test::serial]
    fn credential_store_new_without_home() {
        #[cfg(windows)]
        let var_name = "USERPROFILE";
        #[cfg(not(windows))]
        let var_name = "HOME";

        let original = std::env::var_os(var_name);
        unsafe { std::env::remove_var(var_name) };
        let result = CredentialStore::new();
        // Restore immediately
        if let Some(val) = original {
            unsafe { std::env::set_var(var_name, val) };
        }
        assert!(result.is_err());
    }

    // Place a directory at the temp-file path so tokio::fs::write fails,
    // then verify the original credential file is untouched.
    #[tokio::test]
    async fn mid_write_failure_preserves_existing_credentials() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let store = CredentialStore::with_dir(dir.path().to_path_buf());

        store
            .set("provider_a", "original-key", ApiKind::Messages, "https://example.com")
            .await
            .expect("initial set");

        // Block the temp-file path with a directory so the next write fails
        let tmp_path = dir.path().join("credentials.tmp");
        std::fs::create_dir_all(&tmp_path).expect("create blocking dir");

        let result = store.set("provider_a", "new-key", ApiKind::Messages, "https://example.com").await;
        assert!(result.is_err(), "set should fail when temp path is a directory");

        // Original credential must survive
        let entry = store.get("provider_a").await.expect("get after failed write");
        assert_eq!(entry.key, "original-key");

        // Clean up blocking directory so tempdir drop succeeds
        std::fs::remove_dir(&tmp_path).expect("remove blocking dir");
    }

    #[test]
    #[cfg(windows)]
    #[allow(unsafe_code, clippy::expect_used)]
    fn restrict_windows_permissions_sets_single_ace() {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Foundation::{ERROR_SUCCESS, LocalFree, HLOCAL};
        use windows::Win32::Security::Authorization::{
            GetNamedSecurityInfoW, SE_FILE_OBJECT,
        };
        use windows::Win32::Security::{
            ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
            GetAclInformation, PSECURITY_DESCRIPTOR,
        };
        use windows::core::PCWSTR;

        let dir = tempfile::tempdir().expect("create tempdir");
        let file = dir.path().join("secret_test");
        std::fs::write(&file, "secret data").expect("write test file");

        restrict_windows_permissions(&file).expect("restrict permissions");

        // Read back the DACL
        let path_wide: Vec<u16> = file
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut dacl_ptr: *mut windows::Win32::Security::ACL = std::ptr::null_mut();
        let mut sd = PSECURITY_DESCRIPTOR::default();

        let result = unsafe {
            GetNamedSecurityInfoW(
                PCWSTR(path_wide.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&raw mut dacl_ptr),
                None,
                &raw mut sd,
            )
        };
        assert_eq!(result, ERROR_SUCCESS, "GetNamedSecurityInfoW failed");

        // Verify DACL has exactly 1 ACE
        let mut acl_info = ACL_SIZE_INFORMATION::default();
        unsafe {
            GetAclInformation(
                dacl_ptr,
                std::ptr::from_mut(&mut acl_info).cast(),
                u32::try_from(std::mem::size_of::<ACL_SIZE_INFORMATION>()).expect("size fits u32"),
                AclSizeInformation,
            )
        }
        .expect("GetAclInformation");

        assert_eq!(
            acl_info.AceCount, 1,
            "DACL should have exactly 1 ACE (current user only)"
        );

        // Cleanup: free the security descriptor allocated by GetNamedSecurityInfoW
        if !sd.0.is_null() {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(sd.0)));
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn restrict_windows_permissions_nonexistent_path_returns_error() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("create tempdir: {e}"));
        let nonexistent = dir.path().join("does_not_exist");

        let err = restrict_windows_permissions(&nonexistent)
            .expect_err("should fail on nonexistent path");

        match &err {
            CredentialError::InvalidFormat(msg) => {
                assert!(
                    msg.contains("SetNamedSecurityInfoW"),
                    "error should reference SetNamedSecurityInfoW, got: {msg}"
                );
            }
            other => panic!("expected InvalidFormat, got: {other:?}"),
        }
    }
}
