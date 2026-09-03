mod common;

use axum::http::{header, StatusCode};
use tower::ServiceExt;

use common::{get_admin_csrf, response_body_string, setup, setup_allow_direct, with_proxy_token};

fn plain_req(method: &str, uri: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .body(axum::body::Body::empty())
        .unwrap()
}

fn wrong_token_req(method: &str, uri: &str) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header(
            shortener::web::security::PROXY_TOKEN_HEADER,
            "forged-or-stale-token-0000000000000000",
        )
        .body(axum::body::Body::empty())
        .unwrap()
}

#[tokio::test]
async fn management_rejects_missing_proxy_token() {
    let app = setup().await;
    // Every management route fails closed without the proxy proof, including
    // read-only GETs: exposing port 3000 must not leak link data.
    for (method, uri) in [
        ("GET", "/admin"),
        ("GET", "/admin/links/some-slug"),
        ("POST", "/admin/links"),
        ("POST", "/admin/links/some-slug"),
        ("POST", "/admin/links/some-slug/disable"),
        ("POST", "/admin/links/some-slug/enable"),
        ("GET", "/api/v1/links"),
        ("GET", "/api/v1/links/some-slug"),
        ("POST", "/api/v1/links"),
        ("PATCH", "/api/v1/links/some-slug"),
        ("DELETE", "/api/v1/links/some-slug"),
        ("POST", "/api/v1/links/some-slug/enable"),
    ] {
        let res = app
            .router
            .clone()
            .oneshot(plain_req(method, uri))
            .await
            .unwrap();
        let (status, headers, _) = response_body_string(res).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert_eq!(
            headers.get(header::CACHE_CONTROL).unwrap(),
            "no-store",
            "{method} {uri} 401 must not be cached"
        );
    }

    // API 401s keep the JSON error envelope.
    let res = app
        .router
        .clone()
        .oneshot(plain_req("GET", "/api/v1/links"))
        .await
        .unwrap();
    let (_, _, body) = response_body_string(res).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], 401);
}

#[tokio::test]
async fn management_rejects_wrong_proxy_token() {
    let app = setup().await;
    for (method, uri) in [
        ("GET", "/admin"),
        ("GET", "/api/v1/links"),
        ("POST", "/api/v1/links"),
    ] {
        let res = app
            .router
            .clone()
            .oneshot(wrong_token_req(method, uri))
            .await
            .unwrap();
        let (status, _, _) = response_body_string(res).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
    // The real token still works, proving the rejection was about the value.
    let res = app
        .router
        .clone()
        .oneshot(
            with_proxy_token(
                axum::http::Request::builder()
                    .method("GET")
                    .uri("/api/v1/links"),
            )
            .body(axum::body::Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn management_ignores_forged_identity_headers() {
    let app = setup().await;
    // X-Authenticated-User is informational only: presenting it without the
    // proxy token must not authenticate.
    let req = axum::http::Request::builder()
        .uri("/admin")
        .header("X-Authenticated-User", "admin")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Likewise, Basic credentials mean nothing to the app; only Caddy
    // evaluates them. Direct callers cannot trade them for access.
    let req = axum::http::Request::builder()
        .uri("/api/v1/links")
        .header("Authorization", "Basic YWRtaW46ZmFrZS1wYXNzd29yZA==")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn management_allows_correct_proxy_token_and_keeps_csrf() {
    let app = setup().await;

    // Read paths work with the token.
    for uri in ["/admin", "/api/v1/links"] {
        let req = with_proxy_token(axum::http::Request::builder().method("GET").uri(uri))
            .body(axum::body::Body::empty())
            .unwrap();
        let res = app.router.clone().oneshot(req).await.unwrap();
        let (status, _, _) = response_body_string(res).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}");
    }

    // Mutations work with token + valid CSRF (auth passes, CSRF still applies).
    let (csrf, cookie) = get_admin_csrf(&app).await;
    let req = with_proxy_token(
        axum::http::Request::builder()
            .method("POST")
            .uri("/admin/links"),
    )
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .header(header::COOKIE, format!("csrf_token={cookie}"))
    .header(header::ORIGIN, "http://localhost")
    .body(axum::body::Body::from(format!(
        "target_url=https%3A%2F%2Fexample.com%2Fviaproxy&custom_slug=viaproxy&csrf_token={csrf}"
    )))
    .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // Token present but CSRF invalid: authentication passes, CSRF still rejects.
    let req = with_proxy_token(
        axum::http::Request::builder()
            .method("POST")
            .uri("/admin/links"),
    )
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .header(header::COOKIE, format!("csrf_token={cookie}"))
    .header(header::ORIGIN, "http://localhost")
    .body(axum::body::Body::from(
        "target_url=https%3A%2F%2Fexample.com%2Fx&csrf_token=v1.1.bad.bad",
    ))
    .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn public_routes_stay_public_without_proxy_token() {
    let app = setup().await;
    for uri in ["/", "/robots.txt", "/health/live", "/health/ready"] {
        let res = app
            .router
            .clone()
            .oneshot(plain_req("GET", uri))
            .await
            .unwrap();
        let (status, _, _) = response_body_string(res).await;
        assert_eq!(status, StatusCode::OK, "GET {uri}");
    }

    // Redirects need no token either.
    let slug = common::create_link(
        &app.state,
        Some("pubredir"),
        "https://example.com/pub",
        None,
    )
    .await;
    let res = app
        .router
        .clone()
        .oneshot(plain_req("GET", &format!("/{slug}")))
        .await
        .unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(
        headers.get(header::LOCATION).unwrap(),
        "https://example.com/pub"
    );
}

#[tokio::test]
async fn loopback_bypass_allows_direct_dev_access() {
    // Mirrors direct `cargo run` ergonomics: development + explicit flag +
    // loopback bind means no proxy token is needed.
    let app = setup_allow_direct().await;
    let res = app
        .router
        .clone()
        .oneshot(plain_req("GET", "/admin"))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);

    let res = app
        .router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/links")
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-Requested-With", "XMLHttpRequest")
                .body(axum::body::Body::from(
                    r#"{"target_url":"https://example.com/direct","custom_slug":"direct1"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // Without the flag the same requests fail closed.
    let locked = setup().await;
    let res = locked
        .router
        .clone()
        .oneshot(plain_req("GET", "/admin"))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn caddyfile_keeps_edge_authentication() {
    // In-repo guard for "Caddy still requires Basic Auth": the committed
    // Caddyfile must keep the management matcher, Basic Auth, Authorization
    // stripping, and proxy-token injection. Live behavior is covered by
    // scripts/smoke-caddy-auth.sh and the CI compose smoke test.
    let caddyfile = std::fs::read_to_string("Caddyfile").expect("tests run from the crate root");
    for required in [
        "@management path /admin /admin/* /api /api/*",
        "basic_auth @management",
        "header_up -Authorization",
        // The assignment replaces client-supplied values, so no separate
        // deletion directive is needed (or wanted).
        "header_up X-Crabhop-Proxy-Token {$UPSTREAM_AUTH_TOKEN}",
    ] {
        assert!(
            caddyfile.contains(required),
            "Caddyfile must contain: {required}"
        );
    }
}
