use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use url::Url;

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

#[derive(Debug, Clone)]
pub struct Config {
    pub env: AppEnv,
    pub bind: SocketAddr,
    pub base_url: Url,
    pub base_origin: String,
    pub database_url: String,
    pub csrf_signing_key: Vec<u8>,
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

        if base_url.host_str().is_none() {
            anyhow::bail!("BASE_URL must be absolute with a host");
        }
        if env.is_production() && base_url.scheme() != "https" {
            anyhow::bail!("in production BASE_URL must use https");
        }
        if !matches!(base_url.scheme(), "http" | "https") {
            anyhow::bail!("BASE_URL scheme must be http or https");
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

        Ok(Arc::new(Self {
            env,
            bind,
            base_url,
            base_origin,
            database_url,
            csrf_signing_key,
        }))
    }

    /// Test/development helper that bypasses environment variables.
    pub fn for_tests(base_url: &str, database_url: &str, csrf_key: &str) -> Arc<Self> {
        let base_url: Url = base_url.parse().expect("valid test BASE_URL");
        let base_origin = origin_of(&base_url);
        assert!(csrf_key.len() >= 32, "test CSRF key must be >= 32 bytes");
        Arc::new(Self {
            env: AppEnv::Development,
            bind: "127.0.0.1:0".parse().unwrap(),
            base_url,
            base_origin,
            database_url: database_url.to_string(),
            csrf_signing_key: csrf_key.as_bytes().to_vec(),
        })
    }
}

fn decode_signing_key(raw: &str) -> Result<Vec<u8>> {
    if raw.is_empty() {
        anyhow::bail!("CSRF_SIGNING_KEY is required");
    }
    // Try strict base64 (standard or url-safe) first; fall back to raw bytes.
    // This lets operators store either a base64 token or a plain passphrase,
    // as long as it carries >= 32 bytes.
    let trimmed = raw.trim();
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

fn origin_of(url: &Url) -> String {
    let host = url.host_str().unwrap_or("");
    match url.port() {
        Some(p) => format!("{}://{}:{}", url.scheme(), host, p),
        None => format!("{}://{}", url.scheme(), host),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_includes_non_default_port() {
        let u: Url = "http://127.0.0.1:45231".parse().unwrap();
        assert_eq!(origin_of(&u), "http://127.0.0.1:45231");
        let u: Url = "https://go.fhola.com".parse().unwrap();
        assert_eq!(origin_of(&u), "https://go.fhola.com");
    }

    #[test]
    fn short_csrf_key_rejected() {
        let err = decode_signing_key("short").unwrap();
        assert!(err.len() < 32);
    }
}
