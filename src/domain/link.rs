//! Domain validation and state transitions. No HTTP or SQL here so the
//! rules can be unit-tested directly.

use rand::Rng;
use url::Url;

use crate::error::AppError;

/// Lowercase, unambiguous alphabet: no 0/1/i/l/o. 31 chars.
pub const SLUG_ALPHABET: &[u8] = b"23456789abcdefghjkmnpqrstuvwxyz";
pub const GENERATED_SLUG_LEN: usize = 10;
pub const MAX_CREATE_RETRIES: u32 = 10;

pub const RESERVED_SLUGS: &[&str] = &[
    "admin",
    "api",
    "health",
    "healthz",
    "metrics",
    "robots.txt",
    "favicon.ico",
    "assets",
    "static",
];

#[derive(Debug, Clone)]
pub struct CreateLinkInput {
    pub target_url: String,
    pub custom_slug: Option<String>,
    pub label: Option<String>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateLinkInput {
    pub target_url: Option<String>,
    pub label: Option<Option<String>>,
    pub expires_at: Option<Option<i64>>,
}

/// Validate and normalize a destination URL.
///
/// Returns the canonical serialized URL on success.
pub fn validate_target_url(raw: &str, base_url: &Url) -> Result<String, AppError> {
    if raw != raw.trim() {
        return Err(AppError::Validation(
            "target URL must not have leading or trailing whitespace".to_string(),
        ));
    }
    if raw.is_empty() {
        return Err(AppError::Validation(
            "target URL must not be empty".to_string(),
        ));
    }
    if raw.len() > 4096 {
        return Err(AppError::Validation(
            "target URL must be at most 4096 bytes".to_string(),
        ));
    }
    if raw.chars().any(|c| c.is_control()) || raw.contains(['\r', '\n']) {
        return Err(AppError::Validation(
            "target URL must not contain control characters".to_string(),
        ));
    }
    let url: Url = raw
        .parse()
        .map_err(|_| AppError::Validation("target URL is not a valid absolute URL".to_string()))?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(AppError::Validation(
            "target URL scheme must be http or https".to_string(),
        ));
    }
    if url.host_str().is_none() {
        return Err(AppError::Validation(
            "target URL must have a valid host".to_string(),
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AppError::Validation(
            "target URL must not contain credentials".to_string(),
        ));
    }
    // Loop protection: never allow a destination that points back at the
    // shortener itself (same host), regardless of slug.
    if let (Some(target_host), Some(base_host)) = (url.host_str(), base_url.host_str()) {
        if target_host.eq_ignore_ascii_case(base_host) {
            // Compare ports too: same host (irrespective of port) is rejected
            // to keep the rule simple and loop-free.
            return Err(AppError::Validation(
                "target URL must not point back to this shortener".to_string(),
            ));
        }
    }

    Ok(url.to_string())
}

/// Normalize (lowercase) and validate a custom slug.
pub fn normalize_custom_slug(raw: &str) -> Result<String, AppError> {
    if raw != raw.trim() {
        return Err(AppError::Validation(
            "slug must not have leading or trailing whitespace".to_string(),
        ));
    }
    if raw.is_empty() {
        return Err(AppError::Validation("slug must not be empty".to_string()));
    }
    let slug = raw.to_ascii_lowercase();
    if slug.len() < 3 || slug.len() > 64 {
        return Err(AppError::Validation(
            "slug must be 3-64 characters".to_string(),
        ));
    }
    let ok = slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !ok {
        return Err(AppError::Validation(
            "slug may only contain a-z, 0-9, _ and -".to_string(),
        ));
    }
    if RESERVED_SLUGS.contains(&slug.as_str()) {
        return Err(AppError::Validation("slug is reserved".to_string()));
    }
    Ok(slug)
}

/// True when `slug` is reserved (case-insensitive) or would collide with a
/// static application route.
pub fn is_reserved_slug(slug: &str) -> bool {
    let lower = slug.to_ascii_lowercase();
    RESERVED_SLUGS.contains(&lower.as_str())
}

pub fn generate_slug() -> String {
    let mut rng = rand::rng();
    (0..GENERATED_SLUG_LEN)
        .map(|_| {
            let idx = rng.random_range(0..SLUG_ALPHABET.len());
            SLUG_ALPHABET[idx] as char
        })
        .collect()
}

/// Validate an optional label (free text, stored as-is, escaped on render).
pub fn validate_label(raw: Option<&str>) -> Result<Option<String>, AppError> {
    match raw {
        None => Ok(None),
        Some(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Ok(None);
            }
            if t.len() > 256 {
                return Err(AppError::Validation(
                    "label must be at most 256 characters".to_string(),
                ));
            }
            if t.chars().any(|c| c.is_control() && c != '\t') {
                return Err(AppError::Validation(
                    "label must not contain control characters".to_string(),
                ));
            }
            Ok(Some(t.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        "https://go.example.com".parse().unwrap()
    }

    #[test]
    fn accepts_valid_https_url() {
        let out = validate_target_url("https://example.com/a/long/path?q=1", &base()).unwrap();
        assert!(out.starts_with("https://example.com/"));
    }

    #[test]
    fn rejects_non_http_scheme() {
        assert!(validate_target_url("ftp://example.com/x", &base()).is_err());
        assert!(validate_target_url("javascript:alert(1)", &base()).is_err());
    }

    #[test]
    fn rejects_credentials_and_controls() {
        assert!(validate_target_url("https://user:pass@example.com/", &base()).is_err());
        assert!(validate_target_url("https://example.com/a\rb", &base()).is_err());
        assert!(validate_target_url("https://example.com/a\nb", &base()).is_err());
    }

    #[test]
    fn rejects_self_loops() {
        assert!(validate_target_url("https://go.example.com/example", &base()).is_err());
        assert!(validate_target_url("https://go.example.com/", &base()).is_err());
    }

    #[test]
    fn rejects_relative_and_overlong() {
        assert!(validate_target_url("/relative/path", &base()).is_err());
        let long = format!("https://example.com/{}", "a".repeat(5000));
        assert!(validate_target_url(&long, &base()).is_err());
    }

    #[test]
    fn custom_slug_rules() {
        assert_eq!(normalize_custom_slug("Example-1_2").unwrap(), "example-1_2");
        assert!(normalize_custom_slug("  abc  ").is_err());
        assert!(normalize_custom_slug("ab").is_err());
        assert!(normalize_custom_slug("has space").is_err());
        assert!(normalize_custom_slug("UPPER OK").is_err());
        assert!(normalize_custom_slug("admin").is_err());
        assert!(normalize_custom_slug("ADMIN").is_err());
        assert!(normalize_custom_slug("health").is_err());
    }

    #[test]
    fn generated_slug_shape() {
        for _ in 0..200 {
            let s = generate_slug();
            assert_eq!(s.len(), GENERATED_SLUG_LEN);
            assert!(s.chars().all(|c| SLUG_ALPHABET.contains(&(c as u8))));
            assert!(!s.contains('0'));
            assert!(!s.contains('1'));
            assert!(!s.contains('i'));
            assert!(!s.contains('l'));
            assert!(!s.contains('o'));
        }
    }
}
