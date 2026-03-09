use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum FlickError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Provider(#[from] ProviderError),

    #[error(transparent)]
    Credential(#[from] CredentialError),

    #[error("invalid arguments: {0}")]
    InvalidArguments(String),

    #[error("invalid tool results: {0}")]
    InvalidToolResults(String),

    #[error("context message limit exceeded ({0})")]
    ContextOverflow(usize),

    #[error("no query provided (use --query or pipe to stdin)")]
    NoQuery,

    #[error("context parse error: {0}")]
    ContextParse(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("tool result parse error: {0}")]
    ToolResultParse(String),
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
            Self::InvalidArguments(_) => "invalid_arguments",
            Self::InvalidToolResults(_) => "invalid_tool_results",
            Self::ContextOverflow(_) => "context_overflow",
            Self::NoQuery => "no_query",
            Self::ContextParse(_) => "context_parse_error",
            Self::Io(_) => "io_error",
            Self::ToolResultParse(_) => "tool_result_parse_error",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    NotFound(PathBuf),

    #[error("config parse error: {0}")]
    Parse(String),

    #[error("unsupported config format: {0} (expected .yaml, .yml, or .json)")]
    UnsupportedFormat(String),

    #[error("config I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("unknown provider: {0}")]
    UnknownProvider(String),

    #[error("invalid tool config: {0}")]
    InvalidToolConfig(String),

    #[error("invalid model config: {0}")]
    InvalidModelConfig(String),
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

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn display_provider_error_variants() {
        let api = ProviderError::Api {
            status: 500,
            message: "internal".into(),
        };
        assert_eq!(api.to_string(), "API error (500): internal");

        let rp = ProviderError::ResponseParse("bad data".into());
        assert_eq!(rp.to_string(), "response parse error: bad data");

        let rl_with = ProviderError::RateLimited {
            retry_after_ms: Some(3000),
        };
        assert!(rl_with.to_string().contains("3000ms"));

        let rl_without = ProviderError::RateLimited {
            retry_after_ms: None,
        };
        assert_eq!(rl_without.to_string(), "rate limited");

        assert_eq!(
            ProviderError::AuthFailed.to_string(),
            "authentication failed"
        );
    }

    #[test]
    fn display_config_error_variants() {
        let nf = ConfigError::NotFound(PathBuf::from("/missing"));
        assert!(nf.to_string().contains("/missing"));

        let pe = ConfigError::Parse("bad syntax".into());
        assert!(pe.to_string().contains("bad syntax"));

        let uf = ConfigError::UnsupportedFormat("foo.toml".into());
        assert!(uf.to_string().contains("foo.toml"));
        assert!(uf.to_string().contains(".yaml"));

        let up = ConfigError::UnknownProvider("acme".into());
        assert!(up.to_string().contains("acme"));

        let itc = ConfigError::InvalidToolConfig("bad".into());
        assert!(itc.to_string().contains("bad"));

        let imc = ConfigError::InvalidModelConfig("zero".into());
        assert!(imc.to_string().contains("zero"));
    }

    #[test]
    fn display_credential_error_variants() {
        assert!(
            CredentialError::NotFound("openai".into())
                .to_string()
                .contains("openai")
        );
        assert!(
            CredentialError::DecryptionFailed("test".into())
                .to_string()
                .contains("test")
        );
        assert!(
            CredentialError::NoSecretKey(PathBuf::from("/key"))
                .to_string()
                .contains("/key")
        );
        assert!(
            CredentialError::InvalidFormat("bad".into())
                .to_string()
                .contains("bad")
        );
    }

    #[test]
    fn display_flick_error_variants() {
        assert!(FlickError::NoQuery.to_string().contains("no query"));
        assert!(
            FlickError::InvalidArguments("bad".into())
                .to_string()
                .contains("bad")
        );
        assert!(
            FlickError::InvalidToolResults("empty".into())
                .to_string()
                .contains("empty")
        );
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
    fn code_invalid_arguments() {
        let e = FlickError::InvalidArguments("bad".into());
        assert_eq!(e.code(), "invalid_arguments");
    }

    #[test]
    fn code_invalid_tool_results() {
        let e = FlickError::InvalidToolResults("empty".into());
        assert_eq!(e.code(), "invalid_tool_results");
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

    #[tokio::test]
    async fn code_provider_http() {
        let err = reqwest::get("http://[::1]:1")
            .await
            .expect_err("connection should fail");
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
    fn code_tool_result_parse() {
        let e = FlickError::ToolResultParse("bad".into());
        assert_eq!(e.code(), "tool_result_parse_error");
    }

    #[test]
    fn code_context_overflow() {
        let e = FlickError::ContextOverflow(1024);
        assert_eq!(e.code(), "context_overflow");
    }

    #[test]
    fn from_conversions() {
        let ce: FlickError = CredentialError::NotFound("x".into()).into();
        assert!(matches!(ce, FlickError::Credential(_)));

        let cfe: FlickError = ConfigError::UnknownProvider("z".into()).into();
        assert!(matches!(cfe, FlickError::Config(_)));
    }
}
