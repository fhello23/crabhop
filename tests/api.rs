mod common;

use axum::http::{header, StatusCode};
use tower::ServiceExt;

use common::{response_body_string, setup, with_proxy_token};

const API_HDR: &str = "X-Requested-With";

fn api_req(
    method: &str,
    uri: &str,
    json: Option<&str>,
    with_api_header: bool,
) -> axum::http::Request<axum::body::Body> {
    let mut b = with_proxy_token(axum::http::Request::builder().method(method).uri(uri));
    if json.is_some() {
        b = b.header(header::CONTENT_TYPE, "application/json");
    }
    if with_api_header {
        b = b.header(API_HDR, "XMLHttpRequest");
    }
    b.body(axum::body::Body::from(json.unwrap_or("").to_string()))
        .unwrap()
}

#[tokio::test]
async fn api_full_lifecycle() {
    let app = setup().await;

    // Create.
    let res = app
        .router
        .clone()
        .oneshot(api_req(
            "POST",
            "/api/v1/links",
            Some(r#"{"target_url":"https://example.com/a/long/path","custom_slug":"apitest1"}"#),
            true,
        ))
        .await
        .unwrap();
    let (status, headers, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(headers.contains_key(header::LOCATION));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["slug"], "apitest1");
    assert_eq!(v["target_url"], "https://example.com/a/long/path");
    assert_eq!(v["disabled"], false);

    // Get.
    let res = app
        .router
        .clone()
        .oneshot(api_req("GET", "/api/v1/links/apitest1", None, false))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["slug"], "apitest1");

    // Patch target.
    let res = app
        .router
        .clone()
        .oneshot(api_req(
            "PATCH",
            "/api/v1/links/apitest1",
            Some(r#"{"target_url":"https://example.com/updated"}"#),
            true,
        ))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["target_url"], "https://example.com/updated");

    // List contains it.
    let res = app
        .router
        .clone()
        .oneshot(api_req("GET", "/api/v1/links?q=apitest1", None, false))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["total"].as_i64().unwrap() >= 1);
    assert!(v["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["slug"] == "apitest1"));

    // DELETE soft-disables (204).
    let res = app
        .router
        .clone()
        .oneshot(api_req("DELETE", "/api/v1/links/apitest1", None, true))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // API GET still returns the object, flagged disabled.
    let res = app
        .router
        .clone()
        .oneshot(api_req("GET", "/api/v1/links/apitest1", None, false))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["disabled"], true);

    // Public redirect is now 404.
    let req = axum::http::Request::builder()
        .uri("/apitest1")
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Re-enable.
    let res = app
        .router
        .clone()
        .oneshot(api_req("POST", "/api/v1/links/apitest1/enable", None, true))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["disabled"], false);
}

#[tokio::test]
async fn api_rejects_missing_custom_header_and_content_type() {
    let app = setup().await;

    // Missing X-Requested-With.
    let res = app
        .router
        .clone()
        .oneshot(api_req(
            "POST",
            "/api/v1/links",
            Some(r#"{"target_url":"https://example.com/x"}"#),
            false,
        ))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");

    // Wrong content type.
    let req = with_proxy_token(
        axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/links"),
    )
    .header(header::CONTENT_TYPE, "text/plain")
    .header(API_HDR, "XMLHttpRequest")
    .body(axum::body::Body::from(
        r#"{"target_url":"https://example.com/x"}"#,
    ))
    .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // Unknown fields rejected.
    let res = app
        .router
        .clone()
        .oneshot(api_req(
            "POST",
            "/api/v1/links",
            Some(r#"{"target_url":"https://example.com/x","hacker":1}"#),
            true,
        ))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // Consistent JSON error shape.
    let v_body = {
        let res = app
            .router
            .clone()
            .oneshot(api_req("GET", "/api/v1/links/does-not-exist", None, false))
            .await
            .unwrap();
        let (status, _, body) = response_body_string(res).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        body
    };
    let v: serde_json::Value = serde_json::from_str(&v_body).unwrap();
    assert!(v.get("error").is_some());
    assert!(v["error"].get("message").is_some());
}

#[tokio::test]
async fn api_pagination_defaults_and_limits() {
    let app = setup().await;
    for i in 0..5 {
        let slug = format!("page{i}");
        let body = format!(r#"{{"target_url":"https://example.com/{i}","custom_slug":"{slug}"}}"#);
        let res = app
            .router
            .clone()
            .oneshot(api_req("POST", "/api/v1/links", Some(&body), true))
            .await
            .unwrap();
        let (status, _, b) = response_body_string(res).await;
        assert_eq!(status, StatusCode::CREATED, "{b}");
    }
    // per_page=2 clamps paging.
    let res = app
        .router
        .clone()
        .oneshot(api_req(
            "GET",
            "/api/v1/links?page=1&per_page=2",
            None,
            false,
        ))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    assert_eq!(v["per_page"], 2);

    // Oversized per_page clamps to 100 (does not error).
    let res = app
        .router
        .clone()
        .oneshot(api_req("GET", "/api/v1/links?per_page=10000", None, false))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["per_page"].as_u64().unwrap() <= 100);
}

#[tokio::test]
async fn api_conflict_on_duplicate_slug() {
    let app = setup().await;
    let body = r#"{"target_url":"https://example.com/one","custom_slug":"dupslug"}"#;
    let res = app
        .router
        .clone()
        .oneshot(api_req("POST", "/api/v1/links", Some(body), true))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::CREATED);

    let res = app
        .router
        .clone()
        .oneshot(api_req("POST", "/api/v1/links", Some(body), true))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn api_rejects_oversized_body() {
    let app = setup().await;
    // 16 KiB cap: a 20 KiB JSON body must be rejected with 413.
    let big_target = format!("https://example.com/{}", "a".repeat(20 * 1024));
    let body = format!(r#"{{"target_url":"{big_target}"}}"#);
    let res = app
        .router
        .clone()
        .oneshot(api_req("POST", "/api/v1/links", Some(&body), true))
        .await
        .unwrap();
    let (status, _, _) = response_body_string(res).await;
    assert!(
        status == StatusCode::PAYLOAD_TOO_LARGE || status == StatusCode::UNPROCESSABLE_ENTITY,
        "oversized body should be rejected, got {status}"
    );
}

#[tokio::test]
async fn api_rejects_past_expirations_on_create_and_update() {
    let app = setup().await;
    let past = shortener::state::now_millis() - 60_000;

    let create_body = format!(
        r#"{{"target_url":"https://example.com/past","custom_slug":"pastcreate","expires_at":{past}}}"#
    );
    let res = app
        .router
        .clone()
        .oneshot(api_req("POST", "/api/v1/links", Some(&create_body), true))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("future"), "{body}");

    let res = app
        .router
        .clone()
        .oneshot(api_req(
            "POST",
            "/api/v1/links",
            Some(r#"{"target_url":"https://example.com/active","custom_slug":"pastupdate"}"#),
            true,
        ))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    let patch_body = format!(r#"{{"expires_at":{past}}}"#);
    let res = app
        .router
        .clone()
        .oneshot(api_req(
            "PATCH",
            "/api/v1/links/pastupdate",
            Some(&patch_body),
            true,
        ))
        .await
        .unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(body.contains("future"), "{body}");
}
