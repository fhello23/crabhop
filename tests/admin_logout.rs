mod common;

use axum::http::{header, StatusCode};
use tower::ServiceExt;

use common::{response_body_string, setup, with_proxy_token};

#[tokio::test]
async fn admin_logout_needs_proxy_token_clears_csrf_and_links_from_nav() {
    let app = setup().await;

    // Without the proxy proof the logout page fails closed like other
    // management routes.
    let req = axum::http::Request::builder()
        .uri("/admin/logout")
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, _, _) = response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // With the token it renders, expires the CSRF cookie, and stays
    // non-cacheable with the admin CSP.
    let req = with_proxy_token(axum::http::Request::builder().uri("/admin/logout"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, headers, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("logout-button"), "{body}");
    assert!(body.contains("/admin/logout"), "{body}");
    let set_cookie = headers
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(set_cookie.contains("csrf_token="), "{set_cookie}");
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");
    assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert!(headers.contains_key(header::CONTENT_SECURITY_POLICY));

    // The admin list nav links to the logout page.
    let req = with_proxy_token(axum::http::Request::builder().uri("/admin"))
        .body(axum::body::Body::empty())
        .unwrap();
    let (_, _, body) = response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
    assert!(body.contains("href=\"/admin/logout\""), "{body}");
}
