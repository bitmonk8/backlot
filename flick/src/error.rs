use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum FlickError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Provider(#[from] ProviderError),

    #[error(transparent)]
    Credential(#[from] CredentialError),

    #[error(transparent)]
    Tool(#[from] ToolError),

    #[error("context message limit exceeded ({0})")]
    ContextOverflow(usize),

    #[error("iteration limit reached ({0})")]
    IterationLimit(u32),

    #[error("no query provided (use --query or pipe to stdin)")]
    NoQuery,

    #[error("context parse error: {0}")]
    ContextParse(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("sandbox error: {0}")]
    Sandbox(String),
}

impl FlickError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Provider(p) => match p {
                ProviderError::RateLimited { .. } => "rate_limit",
                ProviderError::AuthFailed => "auth_failed",
                ProviderError::Api { .. } => "api_error",
                ProviderError::Http(_) => "provider_http_error",
                ProviderError::ResponseParse(_) => "response_parse_error",
            },
            Self::Config(_) => "config_error",
            Self::Credential(_) => "credential_error",
            Self::Tool(_) => "tool_error",
            Self::ContextOverflow(_) => "context_overflow",
            Self::IterationLimit(_) => "iteration_limit",
            Self::NoQuery => "no_query",
            Self::ContextParse(_) => "context_parse_error",
            Self::Io(_) => "io_error",
            Self::Sandbox(_) => "sandbox_error",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    NotFound(PathBuf),

    #[error("config parse error: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("config I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unknown provider: {0}")]
    UnknownProvider(String),

    #[error("invalid tool config: {0}")]
    InvalidToolConfig(String),

    #[error("invalid model config: {0}")]
    InvalidModelConfig(String),

    #[error("invalid sandbox config: {0}")]
    InvalidSandboxConfig(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("response parse error: {0}")]
    ResponseParse(String),

    #[error("rate limited{}", .retry_after_ms.as_ref().map_or_else(String::new, |ms| format!(" (retry after {ms}ms)")))]
    RateLimited { retry_after_ms: Option<u64> },

    #[error("authentication failed")]
    AuthFailed,
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("credential not found for provider: {0}")]
    NotFound(String),

    #[error("decryption failed for provider: {0}")]
    DecryptionFailed(String),

    #[error("secret key not found at {0}")]
    NoSecretKey(PathBuf),

    #[error("credential store I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid credential format: {0}")]
    InvalidFormat(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("tool not found: {0}")]
    NotFound(String),

    #[error("tool execution failed: {0}")]
    ExecutionFailed(String),

    #[error("path outside allowed resources: {0}")]
    PathDenied(PathBuf),

    #[error("tool I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tool timeout after {0}s")]
    Timeout(u64),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn display_provider_error_variants() {
        let api = ProviderError::Api { status: 500, message: "internal".into() };
        assert_eq!(api.to_string(), "API error (500): internal");

        let rp = ProviderError::ResponseParse("bad data".into());
        assert_eq!(rp.to_string(), "response parse error: bad data");

        let rl_with = ProviderError::RateLimited { retry_after_ms: Some(3000) };
        assert!(rl_with.to_string().contains("3000ms"));

        let rl_without = ProviderError::RateLimited { retry_after_ms: None };
        assert_eq!(rl_without.to_string(), "rate limited");

        assert_eq!(ProviderError::AuthFailed.to_string(), "authentication failed");
    }

    #[test]
    fn display_config_error_variants() {
        let nf = ConfigError::NotFound(PathBuf::from("/missing"));
        assert!(nf.to_string().contains("/missing"));

        let up = ConfigError::UnknownProvider("acme".into());
        assert!(up.to_string().contains("acme"));

        let itc = ConfigError::InvalidToolConfig("bad".into());
        assert!(itc.to_string().contains("bad"));

        let imc = ConfigError::InvalidModelConfig("zero".into());
        assert!(imc.to_string().contains("zero"));

        let isc = ConfigError::InvalidSandboxConfig("bad wrapper".into());
        assert!(isc.to_string().contains("bad wrapper"));
    }

    #[test]
    fn display_credential_error_variants() {
        assert!(CredentialError::NotFound("openai".into()).to_string().contains("openai"));
        assert!(CredentialError::DecryptionFailed("test".into()).to_string().contains("test"));
        assert!(CredentialError::NoSecretKey(PathBuf::from("/key")).to_string().contains("/key"));
        assert!(CredentialError::InvalidFormat("bad".into()).to_string().contains("bad"));
    }

    #[test]
    fn display_tool_error_variants() {
        assert!(ToolError::NotFound("foo".into()).to_string().contains("foo"));
        assert!(ToolError::ExecutionFailed("crash".into()).to_string().contains("crash"));
        assert!(ToolError::PathDenied(PathBuf::from("/secret")).to_string().contains("/secret"));
        assert!(ToolError::Timeout(30).to_string().contains("30"));
    }

    #[test]
    fn display_flick_error_variants() {
        assert!(FlickError::IterationLimit(5).to_string().contains('5'));
        assert!(FlickError::NoQuery.to_string().contains("no query"));
    }

    #[test]
    fn code_rate_limit() {
        let e = FlickError::Provider(ProviderError::RateLimited {
            retry_after_ms: Some(1000),
        });
        assert_eq!(e.code(), "rate_limit");
    }

    #[test]
    fn code_auth_failed() {
        let e = FlickError::Provider(ProviderError::AuthFailed);
        assert_eq!(e.code(), "auth_failed");
    }

    #[test]
    fn code_api_error() {
        let e = FlickError::Provider(ProviderError::Api {
            status: 500,
            message: "fail".into(),
        });
        assert_eq!(e.code(), "api_error");
    }

    #[test]
    fn code_response_parse() {
        let e = FlickError::Provider(ProviderError::ResponseParse("bad".into()));
        assert_eq!(e.code(), "response_parse_error");
    }

    #[test]
    fn code_config() {
        let e = FlickError::Config(ConfigError::NotFound("/x".into()));
        assert_eq!(e.code(), "config_error");
    }

    #[test]
    fn code_credential() {
        let e = FlickError::Credential(CredentialError::NotFound("p".into()));
        assert_eq!(e.code(), "credential_error");
    }

    #[test]
    fn code_tool() {
        let e = FlickError::Tool(ToolError::NotFound("t".into()));
        assert_eq!(e.code(), "tool_error");
    }

    #[test]
    fn code_iteration_limit() {
        let e = FlickError::IterationLimit(25);
        assert_eq!(e.code(), "iteration_limit");
    }

    #[test]
    fn code_no_query() {
        let e = FlickError::NoQuery;
        assert_eq!(e.code(), "no_query");
    }

    #[test]
    fn code_io() {
        let e = FlickError::Io(std::io::Error::other("x"));
        assert_eq!(e.code(), "io_error");
    }

    #[test]
    fn code_sandbox() {
        let e = FlickError::Sandbox("wrapper not found".into());
        assert_eq!(e.code(), "sandbox_error");
        assert!(e.to_string().contains("wrapper not found"));
    }

    #[tokio::test]
    async fn code_provider_http() {
        let err = reqwest::get("http://[::1]:1").await.expect_err("connection should fail");
        let e = FlickError::Provider(ProviderError::Http(err));
        assert_eq!(e.code(), "provider_http_error");
    }

    #[test]
    fn code_context_parse() {
        let Err(json_err) = serde_json::from_str::<serde_json::Value>("bad") else {
            panic!("expected parse error");
        };
        let e = FlickError::ContextParse(json_err);
        assert_eq!(e.code(), "context_parse_error");
    }

    #[test]
    fn from_conversions() {
        let ce: FlickError = CredentialError::NotFound("x".into()).into();
        assert!(matches!(ce, FlickError::Credential(_)));

        let te: FlickError = ToolError::NotFound("y".into()).into();
        assert!(matches!(te, FlickError::Tool(_)));

        let cfe: FlickError = ConfigError::UnknownProvider("z".into()).into();
        assert!(matches!(cfe, FlickError::Config(_)));
    }
}
