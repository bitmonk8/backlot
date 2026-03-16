use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::ApiKind;
use crate::config::CompatFlags;
use crate::error::CredentialError;

use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Nonce};
use zeroize::Zeroizing;

const NONCE_LEN: usize = 12;
const PREFIX: &str = "enc3:";

/// Serialized form for a single provider entry in the TOML file.
#[derive(Debug, Serialize, Deserialize)]
struct StoredProvider {
    key: String,
    api: ApiKind,
    base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compat: Option<CompatFlags>,
}

/// Provider info returned by `get()` — contains decrypted key + metadata.
pub struct ProviderInfo {
    pub api: ApiKind,
    pub base_url: String,
    pub key: String,
    pub compat: Option<CompatFlags>,
}

/// Summary info returned by `list()` — no key.
pub struct ProviderSummary {
    pub name: String,
    pub api: ApiKind,
    pub base_url: String,
}

/// Encrypted provider store at `~/.flick/providers` (TOML).
pub struct ProviderRegistry {
    dir: PathBuf,
}

impl ProviderRegistry {
    /// Load from the default `~/.flick/` directory.
    pub fn load_default() -> Result<Self, CredentialError> {
        let dir = flick_dir()?;
        Ok(Self { dir })
    }

    /// Load from an explicit directory (for testing).
    pub const fn load(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Decrypt and return the full provider entry for the given name.
    pub async fn get(&self, name: &str) -> Result<ProviderInfo, CredentialError> {
        let key = self.load_secret_key().await?;
        let providers = self.load_providers_file().await?;
        let stored = providers
            .get(name)
            .ok_or_else(|| CredentialError::NotFound(name.to_string()))?;
        let decrypted_key = decrypt(&key, &stored.key, name)?;
        Ok(ProviderInfo {
            key: decrypted_key,
            api: stored.api,
            base_url: stored.base_url.clone(),
            compat: stored.compat.clone(),
        })
    }

    /// Return sorted provider summaries (no keys).
    pub async fn list(&self) -> Result<Vec<ProviderSummary>, CredentialError> {
        let providers = self.load_providers_file().await?;
        Ok(providers
            .into_iter()
            .map(|(name, stored)| ProviderSummary {
                name,
                api: stored.api,
                base_url: stored.base_url,
            })
            .collect())
    }

    /// Encrypt and store a provider entry.
    pub async fn set(
        &self,
        name: &str,
        api_key: &str,
        api: ApiKind,
        base_url: &str,
        compat: Option<CompatFlags>,
    ) -> Result<(), CredentialError> {
        let key = self.load_or_create_secret_key().await?;
        let encrypted = encrypt(&key, api_key, name)?;

        let mut providers = self.load_providers_file().await?;
        providers.insert(
            name.to_string(),
            StoredProvider {
                key: encrypted,
                api,
                base_url: base_url.to_string(),
                compat,
            },
        );
        self.write_providers_file(&providers).await?;
        Ok(())
    }

    /// Remove a provider entry. Returns true if it existed.
    pub async fn remove(&self, name: &str) -> Result<bool, CredentialError> {
        let mut providers = self.load_providers_file().await?;
        let existed = providers.remove(name).is_some();
        if existed {
            self.write_providers_file(&providers).await?;
        }
        Ok(existed)
    }

    fn secret_key_path(&self) -> PathBuf {
        self.dir.join(".secret_key")
    }

    fn providers_path(&self) -> PathBuf {
        self.dir.join("providers")
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
            Err(CredentialError::NoSecretKey(_)) => {}
            Err(e) => return Err(e),
        }
        let mut key = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(&mut *key);
        tokio::fs::create_dir_all(&self.dir).await?;
        let path = self.secret_key_path();
        let hex_key = Zeroizing::new(hex::encode(*key));

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
                    return self.load_secret_key().await;
                }
                Err(e) => return Err(CredentialError::Io(e)),
            }
        }

        Ok(key)
    }

    async fn load_providers_file(
        &self,
    ) -> Result<BTreeMap<String, StoredProvider>, CredentialError> {
        let path = self.providers_path();
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(e) => return Err(CredentialError::Io(e)),
        };
        toml::from_str::<BTreeMap<String, StoredProvider>>(&text)
            .map_err(|e| CredentialError::InvalidFormat(e.to_string()))
    }

    async fn write_providers_file(
        &self,
        providers: &BTreeMap<String, StoredProvider>,
    ) -> Result<(), CredentialError> {
        let text = toml::to_string(providers)
            .map_err(|e| CredentialError::InvalidFormat(e.to_string()))?;
        let path = self.providers_path();

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
    let home = home_dir()
        .ok_or_else(|| CredentialError::InvalidFormat("HOME/USERPROFILE not set".into()))?;
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

#[cfg(windows)]
#[allow(unsafe_code, clippy::too_many_lines)]
fn restrict_windows_permissions(path: &std::path::Path) -> Result<(), CredentialError> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_SUCCESS, HANDLE, HLOCAL, LocalFree, WIN32_ERROR,
    };
    use windows::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
        OBJECT_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
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

    let token = {
        let mut h = HANDLE::default();
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut h) }
            .map_err(|e| CredentialError::InvalidFormat(format!("OpenProcessToken: {e}")))?;
        h
    };
    let _token_guard = HandleGuard(token);

    let mut needed = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &raw mut needed) };

    if needed == 0 {
        return Err(CredentialError::InvalidFormat(
            "GetTokenInformation probe returned size 0".into(),
        ));
    }

    let align_len = (needed as usize).div_ceil(std::mem::size_of::<u64>());
    let mut aligned: Vec<u64> = vec![0u64; align_len];
    let buffer: &mut [u8] =
        unsafe { std::slice::from_raw_parts_mut(aligned.as_mut_ptr().cast(), needed as usize) };
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

    let user_sid: PSID = unsafe { (*aligned.as_ptr().cast::<TOKEN_USER>()).User.Sid };

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: 0x001F_01FF,
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
        .encrypt(
            nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad: provider.as_bytes(),
            },
        )
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
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad: provider.as_bytes(),
            },
        )
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
    async fn set_then_get_round_trip() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());

        registry
            .set(
                "anthropic",
                "sk-ant-test-key-123",
                ApiKind::Messages,
                "https://api.anthropic.com",
                None,
            )
            .await
            .expect("set should succeed");
        let entry = registry.get("anthropic").await.expect("get should succeed");
        assert_eq!(entry.key, "sk-ant-test-key-123");
        assert_eq!(entry.api, ApiKind::Messages);
        assert_eq!(entry.base_url, "https://api.anthropic.com");
        assert!(entry.compat.is_none());
    }

    #[tokio::test]
    async fn set_with_compat_flags() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());

        registry
            .set(
                "openrouter",
                "key",
                ApiKind::ChatCompletions,
                "https://openrouter.ai",
                Some(CompatFlags {
                    explicit_tool_choice_auto: true,
                }),
            )
            .await
            .expect("set");
        let entry = registry.get("openrouter").await.expect("get");
        let compat = entry.compat.expect("compat should be Some");
        assert!(compat.explicit_tool_choice_auto);
    }

    #[tokio::test]
    async fn list_returns_sorted() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());

        registry
            .set(
                "zebra",
                "k",
                ApiKind::ChatCompletions,
                "https://z.com",
                None,
            )
            .await
            .expect("set");
        registry
            .set(
                "alpha",
                "k",
                ApiKind::ChatCompletions,
                "https://a.com",
                None,
            )
            .await
            .expect("set");

        let providers = registry.list().await.expect("list");
        let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zebra"]);
    }

    #[tokio::test]
    async fn list_empty_when_no_providers() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let providers = registry.list().await.expect("list");
        assert!(providers.is_empty());
    }

    #[tokio::test]
    async fn remove_existing_provider() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());

        registry
            .set(
                "test",
                "key",
                ApiKind::Messages,
                "https://example.com",
                None,
            )
            .await
            .expect("set");
        assert!(registry.remove("test").await.expect("remove"));
        assert!(!registry.remove("test").await.expect("remove again"));
    }

    #[tokio::test]
    async fn get_not_found() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        registry
            .set(
                "existing",
                "key",
                ApiKind::Messages,
                "https://example.com",
                None,
            )
            .await
            .expect("set");
        let result = registry.get("nonexistent").await;
        assert!(matches!(result, Err(CredentialError::NotFound(_))));
    }

    #[test]
    #[allow(unsafe_code)]
    #[serial_test::serial]
    fn load_default_without_home() {
        #[cfg(windows)]
        let var_name = "USERPROFILE";
        #[cfg(not(windows))]
        let var_name = "HOME";

        let original = std::env::var_os(var_name);
        unsafe { std::env::remove_var(var_name) };
        let result = ProviderRegistry::load_default();
        if let Some(val) = original {
            unsafe { std::env::set_var(var_name, val) };
        }
        assert!(result.is_err());
    }
}
