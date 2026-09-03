#![allow(dead_code)]

use std::sync::Arc;

use axum::Router;
use http_body_util::BodyExt;
use shortener::config::Config;
use shortener::domain::link::CreateLinkInput;
use shortener::state::{connect_db, AppState};
use shortener::web::app_router;

pub const TEST_CSRF_KEY: &str = "test-csrf-signing-key-0123456789abcdef";
pub const TEST_BASE_URL: &str = "http://localhost";
pub const TEST_PROXY_TOKEN: &str = "test-proxy-token-0123456789abcdefGH";

pub struct TestApp {
    pub router: Router,
    pub state: AppState,
}

pub async fn setup() -> TestApp {
    setup_with_direct(false).await
}

/// App with the development loopback bypass enabled: management routes work
/// without the proxy token (mirrors direct `cargo run` ergonomics).
pub async fn setup_allow_direct() -> TestApp {
    setup_with_direct(true).await
}

async fn setup_with_direct(allow_direct: bool) -> TestApp {
    let config: Arc<Config> = Config::for_tests(
        TEST_BASE_URL,
        "sqlite::memory:",
        TEST_CSRF_KEY,
        TEST_PROXY_TOKEN,
        allow_direct,
    );
    let db = connect_db(&config.database_url)
        .await
        .expect("test database connects and migrates");
    let state = AppState::new(db, config);
    let router = app_router(state.clone());
    TestApp { router, state }
}

/// Attach the proxy proof header required by /admin and /api.
pub fn with_proxy_token(b: axum::http::request::Builder) -> axum::http::request::Builder {
    b.header(
        shortener::web::security::PROXY_TOKEN_HEADER,
        TEST_PROXY_TOKEN,
    )
}

pub async fn create_link(
    state: &AppState,
    slug: Option<&str>,
    target: &str,
    expires_at: Option<i64>,
) -> String {
    let input = CreateLinkInput {
        target_url: target.to_string(),
        custom_slug: slug.map(|s| s.to_string()),
        label: None,
        expires_at,
    };
    let link = shortener::db::links::create_link(&state.db, &state.config.base_url, input)
        .await
        .expect("test link creation succeeds");
    link.slug
}

pub async fn response_body_string(
    res: axum::response::Response,
) -> (axum::http::StatusCode, axum::http::HeaderMap, String) {
    let (mut parts, body) = res.into_parts();
    let headers = std::mem::take(&mut parts.headers);
    let bytes = body.collect().await.expect("collect body").to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    (parts.status, headers, text)
}

pub fn extract_csrf_token(html: &str) -> Option<String> {
    // Looks for name="csrf_token" value="...".
    let marker = "name=\"csrf_token\"";
    let idx = html.find(marker)?;
    let rest = &html[idx + marker.len()..];
    let v_idx = rest.find("value=\"")?;
    let start = idx + marker.len() + v_idx + "value=\"".len();
    let end = html[start..].find('"')?;
    Some(html[start..start + end].to_string())
}

pub fn extract_set_cookie_token(headers: &axum::http::HeaderMap) -> Option<String> {
    for value in headers.get_all(axum::http::header::SET_COOKIE).iter() {
        let s = value.to_str().ok()?;
        for part in s.split(';') {
            let part = part.trim();
            if let Some(v) = part.strip_prefix("csrf_token=") {
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Perform a GET /admin to obtain a fresh (token, cookie) pair.
pub async fn get_admin_csrf(app: &TestApp) -> (String, String) {
    use tower::ServiceExt;
    let req = with_proxy_token(axum::http::Request::builder().method("GET").uri("/admin"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (_status, headers, body) = response_body_string(res).await;
    let token = extract_csrf_token(&body).expect("admin page contains csrf token");
    let cookie = extract_set_cookie_token(&headers).unwrap_or_else(|| token.clone());
    (token, cookie)
}
