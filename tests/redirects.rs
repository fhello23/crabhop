mod common;

use axum::http::{header, StatusCode};
use tower::ServiceExt;

use common::{create_link, response_body_string, setup};

fn redirect_req(method: &str, slug: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(format!("/{slug}"))
        .body(axum::body::Body::empty())
        .unwrap()
}

#[tokio::test]
async fn active_link_redirects_with_correct_headers() {
    let app = setup().await;
    let slug = create_link(
        &app.state,
        Some("example"),
        "https://example.com/a/long/path",
        None,
    )
    .await;

    for method in ["GET", "HEAD"] {
        let res = app
            .router
            .clone()
            .oneshot(redirect_req(method, &slug))
            .await
            .unwrap();
        let (status, headers, _body) = response_body_string(res).await;
        assert_eq!(status, StatusCode::FOUND, "method {method}");
        assert_eq!(
            headers.get(header::LOCATION).unwrap(),
            "https://example.com/a/long/path"
        );
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        assert_eq!(headers.get("X-Robots-Tag").unwrap(), "noindex, nofollow");
    }
}

#[tokio::test]
async fn unknown_slug_returns_404() {
    let app = setup().await;
    let res = app
        .router
        .clone()
        .oneshot(redirect_req("GET", "nope-missing"))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn disabled_link_returns_404() {
    let app = setup().await;
    let slug = create_link(
        &app.state,
        Some("tobedisabled"),
        "https://example.com/x",
        None,
    )
    .await;
    shortener::db::links::set_disabled(&app.state.db, &slug, true)
        .await
        .unwrap();
    let res = app
        .router
        .clone()
        .oneshot(redirect_req("GET", &slug))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn expired_link_returns_410() {
    let app = setup().await;
    let future = shortener::state::now_millis() + 60_000;
    let slug = create_link(
        &app.state,
        Some("oldlink"),
        "https://example.com/old",
        Some(future),
    )
    .await;

    // Public writes reject past expirations. Move this fixture into the past
    // directly so redirect behavior for previously-valid links is still tested.
    let past = shortener::state::now_millis() - 1_000;
    sqlx::query("UPDATE links SET expires_at = ? WHERE slug = ?")
        .bind(past)
        .bind(&slug)
        .execute(&app.state.db)
        .await
        .unwrap();
    let res = app
        .router
        .clone()
        .oneshot(redirect_req("GET", &slug))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::GONE);
}

#[tokio::test]
async fn reserved_routes_win_over_slug_lookup() {
    let app = setup().await;
    // These must never be interpreted as slugs, even though a slug lookup
    // would return 404 anyway — the point is they return their own
    // application responses, not redirect logic.
    for (uri, expected) in [
        ("/robots.txt", StatusCode::OK),
        ("/health/live", StatusCode::OK),
        ("/health/ready", StatusCode::OK),
        ("/static/app.css", StatusCode::OK),
    ] {
        let req = axum::http::Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let res = app.router.clone().oneshot(req).await.unwrap();
        let (status, _, _) = response_body_string(res).await;
        assert_eq!(status, expected, "route {uri}");
    }
    // Management routes fail closed at the application boundary even though
    // no slug lookup is involved: direct access without the proxy token is
    // rejected before any handler runs.
    let req = axum::http::Request::builder()
        .uri("/admin")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Reserved slugs cannot be created via the domain layer.
    let base: url::Url = "http://localhost".parse().unwrap();
    for reserved in ["admin", "api", "health", "static", "assets"] {
        let input = shortener::domain::link::CreateLinkInput {
            target_url: "https://example.com/ok".to_string(),
            custom_slug: Some(reserved.to_string()),
            label: None,
            expires_at: None,
        };
        let r = shortener::db::links::create_link(&app.state.db, &base, input).await;
        assert!(r.is_err(), "reserved slug {reserved} must be rejected");
    }
}

#[tokio::test]
async fn invalid_destinations_and_loops_rejected() {
    let app = setup().await;
    let base = app.state.config.base_url.clone();
    // Header injection / control chars.
    for bad in [
        "https://example.com/a\rb",
        "https://example.com/a\nb",
        "ftp://example.com/x",
        "https://user:pass@example.com/",
        "/relative/path",
    ] {
        assert!(
            shortener::domain::link::validate_target_url(bad, &base).is_err(),
            "must reject {bad:?}"
        );
    }
    // Loop back to the shortener itself.
    assert!(
        shortener::domain::link::validate_target_url("http://localhost/some-slug", &base).is_err()
    );
}

#[tokio::test]
async fn health_endpoints_behave() {
    let app = setup().await;
    let req = axum::http::Request::builder()
        .uri("/health/live")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("live"));

    let req = axum::http::Request::builder()
        .uri("/health/ready")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("ready"));
}

#[tokio::test]
async fn redirect_never_fetches_target() {
    // By construction the handler only issues a Location header; there is no
    // HTTP client in the dependency tree path for redirects. This test pins
    // the observable behavior: empty body + Location, no proxying.
    let app = setup().await;
    let slug = create_link(
        &app.state,
        Some("nofetch"),
        "https://example.com/never-fetched",
        None,
    )
    .await;
    let res = app
        .router
        .clone()
        .oneshot(redirect_req("GET", &slug))
        .await
        .unwrap();
    let (status, headers, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FOUND);
    assert!(headers.contains_key(header::LOCATION));
    assert!(body.is_empty() || !body.contains("never-fetched"));
}
