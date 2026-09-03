//! CSRF tokens, origin verification, and security headers.
//!
//! CSRF design (double-submit, stateless):
//! - Token format: `v1.<expiry_millis>.<nonce_b64url>.<sig_b64url>`
//!   where sig = HMAC-SHA256(signing_key, "v1.{expiry}.{nonce}").
//! - GET /admin issues the token and sets it as a cookie; every POST form
//!   echoes it back in a hidden field. Validation requires: both present,
//!   both well-formed, signatures valid (constant-time), not expired, and
//!   equal (constant-time).
//! - Cookie attributes: HttpOnly, SameSite=Strict, Path=/, and Secure when
//!   the base URL is https (always true in production).
//! - Additionally, browser POSTs must carry an Origin (preferred) or Referer
//!   matching the configured BASE_URL origin; requests with neither are
//!   rejected.

use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

pub const CSRF_COOKIE_NAME: &str = "csrf_token";
pub const CSRF_FORM_FIELD: &str = "csrf_token";
pub const CSRF_TTL_MILLIS: i64 = 24 * 60 * 60 * 1000; // 24h
pub const API_CSRF_HEADER: &str = "x-requested-with";

pub fn generate_csrf_token(signing_key: &[u8], now_millis: i64) -> String {
    let expiry = now_millis + CSRF_TTL_MILLIS;
    let mut nonce = [0u8; 32];
    rand::Rng::fill(&mut rand::rng(), &mut nonce);
    let nonce_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(nonce);
    let payload = format!("v1.{expiry}.{nonce_b64}");
    let sig = hmac_sign(signing_key, payload.as_bytes());
    let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig);
    format!("{payload}.{sig_b64}")
}

fn hmac_sign(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(msg);
    mac.finalize().into_bytes().to_vec()
}

fn verify_signature(signing_key: &[u8], payload: &str, sig_b64: &str) -> bool {
    let expected = hmac_sign(signing_key, payload.as_bytes());
    let got = match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(sig_b64) {
        Ok(b) => b,
        Err(_) => return false,
    };
    // Constant-time comparison to avoid leaking signature validity.
    (expected.as_slice().ct_eq(got.as_slice())).into()
}

pub fn verify_csrf_token(signing_key: &[u8], token: &str, now_millis: i64) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 4 || parts[0] != "v1" {
        return false;
    }
    let expiry: i64 = match parts[1].parse() {
        Ok(e) => e,
        Err(_) => return false,
    };
    if expiry <= now_millis {
        return false;
    }
    if parts[2].is_empty() || parts[3].is_empty() {
        return false;
    }
    let payload = format!("v1.{}.{}", parts[1], parts[2]);
    verify_signature(signing_key, &payload, parts[3])
}

/// Validate the double-submit pair: cookie token and form token must both be
/// valid and equal (constant-time).
pub fn validate_csrf_pair(
    signing_key: &[u8],
    cookie_token: Option<&str>,
    form_token: Option<&str>,
    now_millis: i64,
) -> bool {
    match (cookie_token, form_token) {
        (Some(c), Some(f)) => {
            if !verify_csrf_token(signing_key, c, now_millis) {
                return false;
            }
            if !verify_csrf_token(signing_key, f, now_millis) {
                return false;
            }
            (c.as_bytes().ct_eq(f.as_bytes())).into()
        }
        _ => false,
    }
}

pub fn csrf_set_cookie_value(token: &str, secure: bool) -> String {
    // Max-Age matches token TTL (24h). Path=/ so all admin POSTs send it.
    let mut s =
        format!("{CSRF_COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400");
    if secure {
        s.push_str("; Secure");
    }
    s
}

pub fn extract_cookie_token(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=') {
            if k.trim() == name {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Verify Origin (preferred) else Referer against the expected origin.
/// Returns true when the request is acceptable. Missing both => false.
pub fn verify_origin(headers: &HeaderMap, expected_origin: &str) -> bool {
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        return origin.trim() == expected_origin;
    }
    if let Some(referer) = headers.get(header::REFERER).and_then(|v| v.to_str().ok()) {
        // Referer is a full URL; accept when it starts with the origin + "/"
        // or equals the origin exactly.
        return referer == expected_origin || referer.starts_with(&format!("{expected_origin}/"));
    }
    false
}

/// JSON API mutation guard: require a custom header so plain cross-site form
/// posts cannot trigger mutations (no CORS is ever enabled).
pub fn check_api_mutation_headers(headers: &HeaderMap) -> Result<(), crate::error::AppError> {
    match headers.get(API_CSRF_HEADER) {
        Some(v) if !v.as_bytes().is_empty() => Ok(()),
        _ => Err(crate::error::AppError::Forbidden(
            "missing required header: X-Requested-With".to_string(),
        )),
    }
}

pub fn require_json_content_type(headers: &HeaderMap) -> Result<(), crate::error::AppError> {
    match headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        Some(ct) if ct.starts_with("application/json") => Ok(()),
        _ => Err(crate::error::AppError::UnsupportedMediaType(
            "Content-Type must be application/json".to_string(),
        )),
    }
}

/// Global security-headers middleware. Adds hardening headers to every
/// response; adds a restrictive CSP to /admin HTML pages.
pub async fn security_headers_mw(
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_owned();
    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    // Never expose powered-by details; harden MIME sniffing & framing.
    if let Ok(v) = "nosniff".parse() {
        headers.insert(header::X_CONTENT_TYPE_OPTIONS, v);
    }
    if let Ok(v) = "DENY".parse() {
        headers.insert(header::X_FRAME_OPTIONS, v);
    }
    if let Ok(v) = "same-origin".parse() {
        headers.insert(header::REFERRER_POLICY, v);
    }
    if path.starts_with("/admin") {
        if let Ok(v) = "default-src 'self'; style-src 'self' 'unsafe-inline'; \
             script-src 'self'; img-src 'self' data:; object-src 'none'; \
             base-uri 'self'; frame-ancestors 'deny'; form-action 'self'"
            .parse()
        {
            headers.insert(header::CONTENT_SECURITY_POLICY, v);
        }
    }
    res
}

/// Corner handler for oversized bodies surfaced by RequestBodyLimitLayer.
/// Axum already returns 413; this documents the contract.
pub async fn payload_too_large_fallback(
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Response {
    let res = next.run(req).await;
    if res.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response();
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csrf_roundtrip_and_tamper() {
        let key = b"test-signing-key-that-is-long-enough-1234";
        let now = 1_700_000_000_000i64;
        let tok = generate_csrf_token(key, now);
        assert!(verify_csrf_token(key, &tok, now));
        assert!(validate_csrf_pair(key, Some(&tok), Some(&tok), now));
        // Tampered token fails.
        let mut bad = tok.clone();
        bad.push('x');
        assert!(!verify_csrf_token(key, &bad, now));
        // Wrong key fails.
        assert!(!verify_csrf_token(
            b"other-key-that-is-also-long-enough-12",
            &tok,
            now
        ));
        // Expired fails.
        assert!(!verify_csrf_token(key, &tok, now + CSRF_TTL_MILLIS + 1));
        // Mismatched pair fails.
        let tok2 = generate_csrf_token(key, now);
        assert!(!validate_csrf_pair(key, Some(&tok), Some(&tok2), now));
        // Missing side fails.
        assert!(!validate_csrf_pair(key, None, Some(&tok), now));
    }

    #[test]
    fn origin_checks() {
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, "https://go.example.com".parse().unwrap());
        assert!(verify_origin(&h, "https://go.example.com"));
        h.insert(header::ORIGIN, "https://evil.com".parse().unwrap());
        assert!(!verify_origin(&h, "https://go.example.com"));

        let mut h2 = HeaderMap::new();
        h2.insert(
            header::REFERER,
            "https://go.example.com/admin".parse().unwrap(),
        );
        assert!(verify_origin(&h2, "https://go.example.com"));

        let empty = HeaderMap::new();
        assert!(!verify_origin(&empty, "https://go.example.com"));
    }
}
