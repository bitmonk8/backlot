use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::ApiKind;
use crate::crypto::{decrypt, encrypt};
use crate::error::CredentialError;

/// Per-provider quirk flags. All default to false.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompatFlags {
    #[serde(default)]
    pub explicit_tool_choice_auto: bool,
}

use chacha20poly1305::aead::OsRng;
use chacha20poly1305::aead::rand_core::RngCore;
use zeroize::Zeroizing;

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
#[derive(Debug)]
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

fn validate_provider_name(name: &str) -> Result<(), CredentialError> {
    if name.is_empty() {
        return Err(CredentialError::InvalidProviderName(
            "provider name must not be empty".into(),
        ));
    }
    if name.len() > 255 {
        return Err(CredentialError::InvalidProviderName(
            "provider name must not exceed 255 characters".into(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(CredentialError::InvalidProviderName(
            "provider name must contain only [a-zA-Z0-9_-]".into(),
        ));
    }
    Ok(())
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
        validate_provider_name(name)?;
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
        validate_provider_name(name)?;
        let parsed = url::Url::parse(base_url)
            .map_err(|e| CredentialError::InvalidBaseUrl(format!("invalid base_url: {e}")))?;
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(CredentialError::InvalidBaseUrl(
                "base_url must use http:// or https:// scheme".into(),
            ));
        }
        // No explicit host check needed: url::Url::parse enforces that
        // http/https URLs contain a non-empty host (returns EmptyHost error).
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
        validate_provider_name(name)?;
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
        let bytes =
            Zeroizing::new(hex::decode(hex_str.trim()).map_err(|_| {
                CredentialError::InvalidSecretKey("secret key: invalid hex".into())
            })?);
        let key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| CredentialError::InvalidSecretKey("secret key must be 32 bytes".into()))?;
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

        match write_new_secret_key_file(&path, &hex_key).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return self.load_secret_key().await;
            }
            Err(e) => return Err(CredentialError::Io(e)),
        }

        #[cfg(windows)]
        {
            if let Err(e) = crate::platform::restrict_windows_permissions(&path) {
                let _ = tokio::fs::remove_file(&path).await;
                return Err(e);
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
            .map_err(|e| CredentialError::TomlParse(e.to_string()))
    }

    async fn write_providers_file(
        &self,
        providers: &BTreeMap<String, StoredProvider>,
    ) -> Result<(), CredentialError> {
        let text =
            toml::to_string(providers).map_err(|e| CredentialError::TomlParse(e.to_string()))?;
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
            if let Err(e) = crate::platform::restrict_windows_permissions(&tmp_path) {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(e);
            }
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(CredentialError::Io(e));
        }
        Ok(())
    }
}

/// Write a new secret key file atomically. Uses `create_new` to avoid races.
/// On Unix, sets mode 0o600 before writing. Cleans up on write/sync failure.
async fn write_new_secret_key_file(
    path: &std::path::Path,
    hex_key: &str,
) -> Result<(), std::io::Error> {
    use tokio::io::AsyncWriteExt;

    #[cfg(unix)]
    let file_result = {
        tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .await
    };
    #[cfg(windows)]
    let file_result = {
        tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .await
    };

    let mut file = file_result?;
    if let Err(e) = file.write_all(hex_key.as_bytes()).await {
        drop(file);
        let _ = tokio::fs::remove_file(path).await;
        return Err(e);
    }
    if let Err(e) = file.sync_all().await {
        drop(file);
        let _ = tokio::fs::remove_file(path).await;
        return Err(e);
    }
    Ok(())
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

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
    async fn get_before_any_set_returns_no_secret_key() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let result = registry.get("anthropic").await;
        assert!(
            matches!(result, Err(CredentialError::NoSecretKey(_))),
            "expected NoSecretKey, got {result:?}"
        );
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
        assert!(
            matches!(result, Err(CredentialError::NotFound(_))),
            "expected NotFound, got {result:?}"
        );
    }

    #[tokio::test]
    async fn get_with_corrupt_secret_key_invalid_hex() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let key_path = dir.path().join(".secret_key");
        tokio::fs::write(&key_path, "not-valid-hex!!")
            .await
            .expect("write corrupt key");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let result = registry.get("test").await;
        assert!(
            matches!(result, Err(CredentialError::InvalidSecretKey(_))),
            "expected InvalidSecretKey for bad hex, got {result:?}"
        );
    }

    #[tokio::test]
    async fn get_with_corrupt_secret_key_wrong_length() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let key_path = dir.path().join(".secret_key");
        // Valid hex but only 16 bytes (32 hex chars) instead of 32 bytes (64 hex chars)
        tokio::fs::write(&key_path, "aabbccdd00112233aabbccdd00112233")
            .await
            .expect("write short key");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let result = registry.get("test").await;
        assert!(
            matches!(result, Err(CredentialError::InvalidSecretKey(_))),
            "expected InvalidSecretKey for wrong length, got {result:?}"
        );
    }

    #[tokio::test]
    async fn set_rejects_degenerate_url_no_host() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let result = registry
            .set("test", "key", ApiKind::Messages, "https://", None)
            .await;
        assert!(
            matches!(result, Err(CredentialError::InvalidBaseUrl(_))),
            "expected InvalidBaseUrl for no-host URL, got {result:?}"
        );
    }

    #[test]
    fn url_crate_rejects_hostless_http_https() {
        // Document the invariant we rely on: url::Url::parse rejects
        // http/https URLs that lack a host, so no separate host check
        // is needed in set().
        for input in ["https://", "http://"] {
            let err = url::Url::parse(input).expect_err("should reject hostless URL");
            assert_eq!(
                err,
                url::ParseError::EmptyHost,
                "expected EmptyHost for {input:?}, got {err:?}"
            );
        }
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

    #[test]
    fn validate_provider_name_empty() {
        assert!(matches!(
            validate_provider_name(""),
            Err(CredentialError::InvalidProviderName(_))
        ));
    }

    #[test]
    fn validate_provider_name_too_long() {
        let long = "a".repeat(256);
        assert!(matches!(
            validate_provider_name(&long),
            Err(CredentialError::InvalidProviderName(_))
        ));
    }

    #[test]
    fn validate_provider_name_invalid_chars() {
        for bad in &["has space", "has.dot", "slash/bad", "colon:bad", "a@b"] {
            assert!(
                matches!(
                    validate_provider_name(bad),
                    Err(CredentialError::InvalidProviderName(_))
                ),
                "expected error for {bad:?}"
            );
        }
    }

    #[test]
    fn validate_provider_name_valid() {
        for good in &["anthropic", "open-ai", "my_provider", "A1-b2_c3"] {
            assert!(
                validate_provider_name(good).is_ok(),
                "expected ok for {good:?}"
            );
        }
    }

    #[test]
    fn validate_provider_name_max_length() {
        let max = "a".repeat(255);
        assert!(validate_provider_name(&max).is_ok());
    }

    #[tokio::test]
    async fn set_rejects_bad_base_url() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let result = registry
            .set("test", "key", ApiKind::Messages, "ftp://bad.com", None)
            .await;
        assert!(matches!(result, Err(CredentialError::InvalidBaseUrl(_))));
    }

    #[tokio::test]
    async fn set_accepts_http_and_https() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        registry
            .set("a", "key", ApiKind::Messages, "https://ok.com", None)
            .await
            .expect("https should work");
        registry
            .set("b", "key", ApiKind::Messages, "http://ok.com", None)
            .await
            .expect("http should work");
    }

    #[tokio::test]
    async fn set_rejects_invalid_name() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let result = registry
            .set("bad name", "key", ApiKind::Messages, "https://ok.com", None)
            .await;
        assert!(matches!(
            result,
            Err(CredentialError::InvalidProviderName(_))
        ));
    }

    #[tokio::test]
    async fn get_rejects_invalid_name() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let result = registry.get("").await;
        assert!(matches!(
            result,
            Err(CredentialError::InvalidProviderName(_))
        ));
    }

    #[tokio::test]
    async fn remove_rejects_invalid_name() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let registry = ProviderRegistry::load(dir.path().to_path_buf());
        let result = registry.remove("bad/name").await;
        assert!(matches!(
            result,
            Err(CredentialError::InvalidProviderName(_))
        ));
    }
}
