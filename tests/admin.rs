mod common;

use axum::http::{header, StatusCode};
use tower::ServiceExt;

use common::{get_admin_csrf, response_body_string, setup, with_proxy_token};

fn post_admin(
    uri: &str,
    form: &str,
    token: &str,
    cookie: &str,
    origin: Option<&str>,
) -> axum::http::Request<axum::body::Body> {
    let mut b = with_proxy_token(axum::http::Request::builder().method("POST").uri(uri))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("csrf_token={cookie}"));
    if let Some(o) = origin {
        b = b.header(header::ORIGIN, o);
    }
    b.body(axum::body::Body::from(format!("{form}&csrf_token={token}")))
        .unwrap()
}

#[tokio::test]
async fn admin_create_and_edit_lifecycle() {
    let app = setup().await;
    let (token, cookie) = get_admin_csrf(&app).await;

    // Create.
    let req = post_admin(
        "/admin/links",
        "target_url=https%3A%2F%2Fexample.com%2Fhello&custom_slug=lifecycle1&label=Test",
        &token,
        &cookie,
        Some("http://localhost"),
    );
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let loc = headers
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(loc.contains("lifecycle1"), "location: {loc}");

    // Edit form shows values.
    let req = with_proxy_token(axum::http::Request::builder().uri("/admin/links/lifecycle1"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("https://example.com/hello"));
    assert!(body.contains("Expires in UTC"));

    // Update destination.
    let req = post_admin(
        "/admin/links/lifecycle1",
        "target_url=https%3A%2F%2Fexample.com%2Fupdated&label=Test2",
        &token,
        &cookie,
        Some("http://localhost"),
    );
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // Disable.
    let req = post_admin(
        "/admin/links/lifecycle1/disable",
        "",
        &token,
        &cookie,
        Some("http://localhost"),
    );
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // Disabled slug now 404s publicly.
    let req = axum::http::Request::builder()
        .uri("/lifecycle1")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Re-enable.
    let req = post_admin(
        "/admin/links/lifecycle1/enable",
        "",
        &token,
        &cookie,
        Some("http://localhost"),
    );
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let req = axum::http::Request::builder()
        .uri("/lifecycle1")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FOUND);
}

#[tokio::test]
async fn admin_rejects_missing_or_invalid_csrf() {
    let app = setup().await;
    let (token, cookie) = get_admin_csrf(&app).await;

    // Missing token.
    let req = with_proxy_token(
        axum::http::Request::builder()
            .method("POST")
            .uri("/admin/links"),
    )
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .header(header::COOKIE, format!("csrf_token={cookie}"))
    .header(header::ORIGIN, "http://localhost")
    .body(axum::body::Body::from(
        "target_url=https%3A%2F%2Fexample.com%2Fx",
    ))
    .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Mismatched token.
    let req = post_admin(
        "/admin/links",
        "target_url=https%3A%2F%2Fexample.com%2Fx",
        "v1.9999999999999.invalid.invalid",
        &cookie,
        Some("http://localhost"),
    );
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Missing Origin AND Referer.
    let req = with_proxy_token(
        axum::http::Request::builder()
            .method("POST")
            .uri("/admin/links"),
    )
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .header(header::COOKIE, format!("csrf_token={cookie}"))
    .body(axum::body::Body::from(format!(
        "target_url=https%3A%2F%2Fexample.com%2Fx&csrf_token={token}"
    )))
    .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Wrong Origin.
    let req = post_admin(
        "/admin/links",
        "target_url=https%3A%2F%2Fexample.com%2Fx",
        &token,
        &cookie,
        Some("https://evil.com"),
    );
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_escapes_stored_values() {
    let app = setup().await;
    // Label with HTML/JS must be escaped on render, not executed.
    let direct = shortener::db::links::create_link(
        &app.state.db,
        &app.state.config.base_url,
        shortener::domain::link::CreateLinkInput {
            target_url: "https://example.com/?q=1".to_string(),
            custom_slug: Some("escapetest".to_string()),
            label: Some("<script>alert('xss')</script>".to_string()),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(direct.slug, "escapetest");

    let req = with_proxy_token(axum::http::Request::builder().uri("/admin"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("<script>alert('xss')</script>"),
        "raw script must not appear"
    );
    assert!(
        body.contains("&lt;script&gt;") || body.contains("&#"),
        "escaped script expected: {body}"
    );

    // Security headers on admin pages.
    let req = with_proxy_token(axum::http::Request::builder().uri("/admin"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (_, headers, _) = response_body_string(res).await;
    assert_eq!(headers.get("X-Content-Type-Options").unwrap(), "nosniff");
    assert_eq!(headers.get("X-Frame-Options").unwrap(), "DENY");
    assert_eq!(headers.get("Referrer-Policy").unwrap(), "same-origin");
    assert!(headers.contains_key("Content-Security-Policy"));

    // CSRF cookie attributes.
    let req = with_proxy_token(axum::http::Request::builder().uri("/admin"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (_, headers, _) = response_body_string(res).await;
    // First request issues a cookie (no cookie sent). It must carry the
    // required attributes; Secure is conditional on https base.
    if let Some(set_cookie) = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
    {
        assert!(set_cookie.contains("HttpOnly"), "cookie: {set_cookie}");
        assert!(
            set_cookie.contains("SameSite=Strict"),
            "cookie: {set_cookie}"
        );
        assert!(set_cookie.contains("Path=/"), "cookie: {set_cookie}");
    }
}

#[tokio::test]
async fn admin_validation_errors_are_actionable() {
    let app = setup().await;
    let (token, cookie) = get_admin_csrf(&app).await;
    // Invalid scheme.
    let req = post_admin(
        "/admin/links",
        "target_url=ftp%3A%2F%2Fexample.com%2Fx",
        &token,
        &cookie,
        Some("http://localhost"),
    );
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.contains("scheme"), "body should explain: {body}");
}
