mod common;

use axum::http::{header, StatusCode};
use tower::ServiceExt;

use common::{get_admin_csrf, response_body_string, setup, with_proxy_token};

fn post_admin_form(
    uri: &str,
    form: &str,
    token: &str,
    cookie: &str,
) -> axum::http::Request<axum::body::Body> {
    with_proxy_token(axum::http::Request::builder().method("POST").uri(uri))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("csrf_token={cookie}"))
        .header(header::ORIGIN, "http://localhost")
        .body(axum::body::Body::from(format!("{form}&csrf_token={token}")))
        .unwrap()
}

fn api_json(method: &str, uri: &str, json: Option<&str>) -> axum::http::Request<axum::body::Body> {
    let mut b = with_proxy_token(axum::http::Request::builder().method(method).uri(uri));
    if json.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    b = b.header("X-Requested-With", "XMLHttpRequest");
    b.body(axum::body::Body::from(json.unwrap_or("").to_string()))
        .unwrap()
}

fn assert_no_store(headers: &axum::http::HeaderMap, what: &str) {
    assert_eq!(
        headers.get(header::CACHE_CONTROL).unwrap(),
        "no-store",
        "{what} must not be cached"
    );
}

#[tokio::test]
async fn admin_responses_are_non_cacheable() {
    let app = setup().await;
    let (csrf, cookie) = get_admin_csrf(&app).await;

    // 200 list.
    let req = with_proxy_token(axum::http::Request::builder().uri("/admin"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_store(&headers, "GET /admin");

    // 303 create success.
    let req = post_admin_form(
        "/admin/links",
        "target_url=https%3A%2F%2Fexample.com%2Fnc1&custom_slug=nc1",
        &csrf,
        &cookie,
    );
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_no_store(&headers, "POST /admin/links 303");

    // 200 edit form.
    let req = with_proxy_token(axum::http::Request::builder().uri("/admin/links/nc1"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_store(&headers, "GET /admin/links/nc1");

    // 422 validation error re-render.
    let req = post_admin_form(
        "/admin/links",
        "target_url=ftp%3A%2F%2Fexample.com%2Fx",
        &csrf,
        &cookie,
    );
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_no_store(&headers, "POST /admin/links 422");

    // 403 CSRF rejection.
    let req = with_proxy_token(
        axum::http::Request::builder()
            .method("POST")
            .uri("/admin/links"),
    )
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .header(header::ORIGIN, "http://localhost")
    .body(axum::body::Body::from(
        "target_url=https%3A%2F%2Fexample.com%2Fx",
    ))
    .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_no_store(&headers, "POST /admin/links 403");
}

#[tokio::test]
async fn api_responses_are_non_cacheable() {
    let app = setup().await;

    // 200 list.
    let res = app
        .router
        .clone()
        .oneshot(api_json("GET", "/api/v1/links", None))
        .await
        .unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_store(&headers, "GET /api/v1/links");

    // 201 create.
    let res = app
        .router
        .clone()
        .oneshot(api_json(
            "POST",
            "/api/v1/links",
            Some(r#"{"target_url":"https://example.com/nc2","custom_slug":"nc2"}"#),
        ))
        .await
        .unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_no_store(&headers, "POST /api/v1/links 201");

    // 200 get.
    let res = app
        .router
        .clone()
        .oneshot(api_json("GET", "/api/v1/links/nc2", None))
        .await
        .unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_store(&headers, "GET /api/v1/links/nc2");

    // 422 validation error.
    let res = app
        .router
        .clone()
        .oneshot(api_json("POST", "/api/v1/links", Some(r#"{}"#)))
        .await
        .unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_no_store(&headers, "POST /api/v1/links 422");

    // 404 JSON error.
    let res = app
        .router
        .clone()
        .oneshot(api_json("GET", "/api/v1/links/does-not-exist", None))
        .await
        .unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_no_store(&headers, "GET /api/v1/links/404");

    // 404 fallback for unknown /api paths.
    let res = app
        .router
        .clone()
        .oneshot(api_json("GET", "/api/nope", None))
        .await
        .unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_no_store(&headers, "GET /api/nope fallback");

    // 500-path safety: unknown slugs are public, not management — covered
    // elsewhere; here assert the management 401s (no token) are non-cacheable.
    for (method, uri) in [("GET", "/admin"), ("GET", "/api/v1/links")] {
        let req = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        let res = app.router.clone().oneshot(req).await.unwrap();
        let (status, headers, _) = response_body_string(res).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_no_store(&headers, "{method} {uri} 401");
    }
}

#[tokio::test]
async fn public_responses_keep_their_own_cache_behavior() {
    // The no-store rule is scoped to management paths: public behavior is
    // unchanged (redirects already set no-store themselves; landing has none).
    let app = setup().await;
    let req = axum::http::Request::builder()
        .uri("/")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers.get(header::CACHE_CONTROL).is_none()
            || headers.get(header::CACHE_CONTROL).unwrap() != "no-store",
        "landing page must not gain management headers"
    );
}
