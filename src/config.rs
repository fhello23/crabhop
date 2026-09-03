use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use url::Url;

const CSRF_SIGNING_KEY_PLACEHOLDER: &str = "replace-me-with-a-long-random-secret-at-least-32-bytes";
const UPSTREAM_AUTH_TOKEN_PLACEHOLDER: &str = "replace-me-with-a-long-random-upstream-token";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEnv {
    Development,
    Production,
}

impl AppEnv {
    fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "production" | "prod" => Ok(Self::Production),
            other => anyhow::bail!("invalid APP_ENV {other:?}: expected development|production"),
        }
    }

    pub fn is_production(self) -> bool {
        matches!(self, Self::Production)
    }
}

#[derive(Clone)]
pub struct Config {
    pub env: AppEnv,
    pub bind: SocketAddr,
    pub base_url: Url,
    pub base_origin: String,
    pub database_url: String,
    pub csrf_signing_key: Vec<u8>,
    /// Proof that a management request arrived through Caddy. Compared in
    /// constant time against the `X-Crabhop-Proxy-Token` request header.
    /// `None` only in development without a token; management routes then
    /// fail closed unless the explicit loopback bypass applies.
    pub upstream_auth_token: Option<String>,
    /// Direct (non-proxied) management access. True only when ALL hold:
    /// development env, explicitly enabled, loopback bind. Never true in
    /// production.
    pub allow_direct_management: bool,
}

// Secrets must never appear in logs; Debug redacts them.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("env", &self.env)
            .field("bind", &self.bind)
            .field("base_url", &self.base_url)
            .field("base_origin", &self.base_origin)
            .field("database_url", &self.database_url)
            .field("csrf_signing_key", &"<redacted>")
            .field("upstream_auth_token", &"<redacted>")
            .field("allow_direct_management", &self.allow_direct_management)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Arc<Self>> {
        let env_raw = std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
        let env = AppEnv::parse(&env_raw)?;

        let bind_raw = std::env::var("APP_BIND").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
        let bind: SocketAddr = bind_raw
            .parse()
            .with_context(|| format!("invalid APP_BIND {bind_raw:?}"))?;

        let base_raw = std::env::var("BASE_URL").context("BASE_URL is required")?;
        let base_url: Url = base_raw
            .parse()
            .with_context(|| format!("invalid BASE_URL {base_raw:?}"))?;

        validate_base_url_shape(&base_url)?;
        if env.is_production() && base_url.scheme() != "https" {
            anyhow::bail!("in production BASE_URL must use https");
        }

        let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;

        let csrf_raw = std::env::var("CSRF_SIGNING_KEY").unwrap_or_default();
        // Accept raw string or base64; require at least 32 bytes of entropy.
        let csrf_signing_key = decode_signing_key(&csrf_raw)?;
        if env.is_production() && csrf_signing_key.len() < 32 {
            anyhow::bail!("in production CSRF_SIGNING_KEY must be at least 32 bytes");
        }
        if csrf_signing_key.len() < 32 {
            anyhow::bail!(
                "CSRF_SIGNING_KEY must be at least 32 bytes (provide a long random secret)"
            );
        }

        let base_origin = origin_of(&base_url);

        let upstream_raw = std::env::var("UPSTREAM_AUTH_TOKEN").unwrap_or_default();
        let upstream_auth_token = decode_upstream_token(&upstream_raw, env)?;

        let allow_direct_requested = std::env::var("UPSTREAM_AUTH_ALLOW_DIRECT")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let allow_direct_management = direct_bypass_permitted(env, allow_direct_requested, bind);

        Ok(Arc::new(Self {
            env,
            bind,
            base_url,
            base_origin,
            database_url,
            csrf_signing_key,
            upstream_auth_token,
            allow_direct_management,
        }))
    }

    /// Test/development helper that bypasses environment variables.
    pub fn for_tests(
        base_url: &str,
        database_url: &str,
        csrf_key: &str,
        upstream_token: &str,
        allow_direct: bool,
    ) -> Arc<Self> {
        let base_url: Url = base_url.parse().expect("valid test BASE_URL");
        let base_origin = origin_of(&base_url);
        assert!(csrf_key.len() >= 32, "test CSRF key must be >= 32 bytes");
        assert!(
            upstream_token.len() >= 32,
            "test upstream token must be >= 32 bytes"
        );
        let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
        Arc::new(Self {
            env: AppEnv::Development,
            bind,
            base_url,
            base_origin,
            database_url: database_url.to_string(),
            csrf_signing_key: csrf_key.as_bytes().to_vec(),
            upstream_auth_token: Some(upstream_token.to_string()),
            allow_direct_management: direct_bypass_permitted(
                AppEnv::Development,
                allow_direct,
                bind,
            ),
        })
    }
}

/// The direct-access bypass is a development convenience, never a production
/// feature: every condition is required, and production fails the first one.
fn direct_bypass_permitted(env: AppEnv, explicitly_enabled: bool, bind: SocketAddr) -> bool {
    matches!(env, AppEnv::Development) && explicitly_enabled && bind.ip().is_loopback()
}

/// Parse the proxy token. The raw value is compared verbatim against the
/// header Caddy injects, so no base64 decoding happens here — length is
/// enforced on the raw string (`openssl rand -base64 48` yields 64 chars).
fn decode_upstream_token(raw: &str, env: AppEnv) -> Result<Option<String>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        if env.is_production() {
            anyhow::bail!("UPSTREAM_AUTH_TOKEN is required in production");
        }
        return Ok(None);
    }
    if trimmed == UPSTREAM_AUTH_TOKEN_PLACEHOLDER {
        anyhow::bail!("UPSTREAM_AUTH_TOKEN still contains the public example placeholder");
    }
    if trimmed.len() < 32 {
        anyhow::bail!(
            "UPSTREAM_AUTH_TOKEN must be at least 32 characters (generate with `openssl rand -base64 48`)"
        );
    }
    Ok(Some(trimmed.to_string()))
}

fn decode_signing_key(raw: &str) -> Result<Vec<u8>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("CSRF_SIGNING_KEY is required");
    }
    if trimmed == CSRF_SIGNING_KEY_PLACEHOLDER {
        anyhow::bail!("CSRF_SIGNING_KEY still contains the public example placeholder");
    }
    // Try strict base64 (standard or url-safe) first; fall back to raw bytes.
    // This lets operators store either a base64 token or a plain passphrase,
    // as long as it carries >= 32 bytes.
    if let Ok(bytes) = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, trimmed) {
        if bytes.len() >= 32 {
            return Ok(bytes);
        }
    }
    if let Ok(bytes) =
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, trimmed)
    {
        if bytes.len() >= 32 {
            return Ok(bytes);
        }
    }
    Ok(trimmed.as_bytes().to_vec())
}

fn validate_base_url_shape(url: &Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("BASE_URL scheme must be http or https");
    }
    if url.host_str().is_none() {
        anyhow::bail!("BASE_URL must be absolute with a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("BASE_URL must not contain credentials");
    }
    if url.path() != "/" {
        anyhow::bail!("BASE_URL must not contain a path");
    }
    if url.query().is_some() || url.fragment().is_some() {
        anyhow::bail!("BASE_URL must not contain a query or fragment");
    }
    Ok(())
}

fn origin_of(url: &Url) -> String {
    url.origin().ascii_serialization()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_includes_non_default_port() {
        let u: Url = "http://127.0.0.1:45231".parse().unwrap();
        assert_eq!(origin_of(&u), "http://127.0.0.1:45231");
        let u: Url = "https://go.example.com".parse().unwrap();
        assert_eq!(origin_of(&u), "https://go.example.com");
    }

    #[test]
    fn short_csrf_key_rejected() {
        let err = decode_signing_key("short").unwrap();
        assert!(err.len() < 32);
    }

    #[test]
    fn public_csrf_placeholder_rejected() {
        let err = decode_signing_key(CSRF_SIGNING_KEY_PLACEHOLDER).unwrap_err();
        assert!(err.to_string().contains("placeholder"));
    }

    #[test]
    fn base_url_must_be_a_root_http_url() {
        for invalid in [
            "ftp://go.example.com/",
            "https://user:pass@go.example.com/",
            "https://go.example.com/base",
            "https://go.example.com/?query=1",
            "https://go.example.com/#fragment",
        ] {
            let url: Url = invalid.parse().unwrap();
            assert!(
                validate_base_url_shape(&url).is_err(),
                "must reject {invalid}"
            );
        }

        let valid: Url = "https://go.example.com/".parse().unwrap();
        assert!(validate_base_url_shape(&valid).is_ok());
    }

    #[test]
    fn upstream_token_rules() {
        let long_enough = "a-test-upstream-token-0123456789abcdef";
        assert!(decode_upstream_token(long_enough, AppEnv::Production)
            .unwrap()
            .is_some());
        // Placeholder is rejected even though it is long enough.
        assert!(
            decode_upstream_token(UPSTREAM_AUTH_TOKEN_PLACEHOLDER, AppEnv::Production).is_err()
        );
        assert!(
            decode_upstream_token(UPSTREAM_AUTH_TOKEN_PLACEHOLDER, AppEnv::Development).is_err()
        );
        // Short tokens are rejected wherever they appear.
        assert!(decode_upstream_token("short", AppEnv::Production).is_err());
        assert!(decode_upstream_token("short", AppEnv::Development).is_err());
        // Missing token fails closed in production, stays unset in development.
        assert!(decode_upstream_token("", AppEnv::Production).is_err());
        assert!(decode_upstream_token("   ", AppEnv::Production).is_err());
        assert!(decode_upstream_token("", AppEnv::Development)
            .unwrap()
            .is_none());
    }

    #[test]
    fn direct_bypass_never_in_production() {
        let loopback: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        let public: SocketAddr = "0.0.0.0:3000".parse().unwrap();
        // Production forbids the bypass no matter what.
        assert!(!direct_bypass_permitted(AppEnv::Production, true, loopback));
        // Development requires the explicit flag AND a loopback bind.
        assert!(direct_bypass_permitted(AppEnv::Development, true, loopback));
        assert!(!direct_bypass_permitted(
            AppEnv::Development,
            false,
            loopback
        ));
        assert!(!direct_bypass_permitted(AppEnv::Development, true, public));
    }

    #[test]
    fn debug_redacts_secrets() {
        let config = Config::for_tests(
            "http://localhost",
            "sqlite::memory:",
            "test-csrf-signing-key-0123456789abcdef",
            "a-test-upstream-token-0123456789abcdef",
            false,
        );
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("test-csrf-signing-key"), "{rendered}");
        assert!(!rendered.contains("test-upstream-token"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }
}
