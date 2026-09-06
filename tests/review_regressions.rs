mod common;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use common::{response_body_string, setup, with_proxy_token};
use serde_json::json;
use shortener::db::links::{create_link, get_link, update_link};
use shortener::domain::link::{CreateLinkInput, UpdateLinkInput};
use tower::ServiceExt;

#[tokio::test]
async fn concurrent_patches_preserve_independent_fields() {
    let app = setup().await;
    create_link(
        &app.state.db,
        &app.state.config.base_url,
        CreateLinkInput {
            text_content: Some("original".into()),
            target_url: String::new(),
            custom_slug: Some("concurrent".into()),
            label: None,
            expires_at: None,
        },
    )
    .await
    .unwrap();
    // Both futures run against one pooled connection, so the original
    // read/modify/write implementation can read the same stale row twice.
    let (text, label) = tokio::join!(
        update_link(
            &app.state.db,
            &app.state.config.base_url,
            "concurrent",
            UpdateLinkInput {
                text_content: Some("new text".into()),
                ..Default::default()
            }
        ),
        update_link(
            &app.state.db,
            &app.state.config.base_url,
            "concurrent",
            UpdateLinkInput {
                label: Some(Some("new label".into())),
                ..Default::default()
            }
        ),
    );
    text.unwrap();
    label.unwrap();
    let saved = get_link(&app.state.db, "concurrent").await.unwrap();
    assert_eq!(saved.text_content.as_deref(), Some("new text"));
    assert_eq!(saved.label.as_deref(), Some("new label"));
}

#[tokio::test]
async fn out_of_range_expirations_are_rejected_without_mutating() {
    let app = setup().await;
    common::create_link(&app.state, Some("expiry"), "https://example.com", None).await;
    for value in [
        json!(i64::MAX),
        json!(i64::MAX.to_string()),
        json!(253_402_300_800_000i64),
    ] {
        for method in ["POST", "PATCH"] {
            let path = if method == "POST" {
                "/api/v1/links"
            } else {
                "/api/v1/links/expiry"
            };
            let body = if method == "POST" {
                json!({"text_content":"notes", "expires_at":value})
            } else {
                json!({"expires_at":value})
            };
            let req = with_proxy_token(Request::builder().method(method).uri(path))
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-Requested-With", "XMLHttpRequest")
                .body(Body::from(body.to_string()))
                .unwrap();
            let (status, _, body) =
                response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{method}: {body}");
        }
    }
    assert!(get_link(&app.state.db, "expiry")
        .await
        .unwrap()
        .expires_at
        .is_none());

    // The last millisecond of year 9999 still round-trips, and explicit
    // nulls must clear fields even though omitted fields are preserved.
    let latest = 253_402_300_799_999i64;
    let updated = update_link(
        &app.state.db,
        &app.state.config.base_url,
        "expiry",
        UpdateLinkInput {
            label: Some(Some("temporary".into())),
            expires_at: Some(Some(latest)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        shortener::state::millis_to_rfc3339(updated.expires_at.unwrap()),
        "9999-12-31T23:59:59.999Z"
    );
    let cleared = update_link(
        &app.state.db,
        &app.state.config.base_url,
        "expiry",
        UpdateLinkInput {
            label: Some(None),
            expires_at: Some(None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(cleared.label.is_none() && cleared.expires_at.is_none());
}

#[tokio::test]
async fn json_content_type_matches_the_media_type_not_a_prefix() {
    let app = setup().await;
    for (content_type, expected) in [
        ("application/jsonp", StatusCode::UNSUPPORTED_MEDIA_TYPE),
        ("application/json-evil", StatusCode::UNSUPPORTED_MEDIA_TYPE),
        ("Application/JSON; charset=utf-8", StatusCode::CREATED),
        ("application/json; charset=utf-8", StatusCode::CREATED),
    ] {
        let req = with_proxy_token(Request::builder().method("POST").uri("/api/v1/links"))
            .header(header::CONTENT_TYPE, content_type)
            .header("X-Requested-With", "XMLHttpRequest")
            .body(Body::from(r#"{"text_content":"notes"}"#))
            .unwrap();
        let (status, _, body) =
            response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
        assert_eq!(status, expected, "{content_type}: {body}");
    }
}

#[tokio::test]
async fn created_banner_cannot_link_to_an_external_host() {
    let app = setup().await;
    let req = with_proxy_token(Request::builder().uri("/admin?created=%2Fevil.example"))
        .body(Body::empty())
        .unwrap();
    let (_, _, html) = response_body_string(app.router.oneshot(req).await.unwrap()).await;
    assert!(!html.contains("href=\"//evil.example\""));
    assert!(!html.contains("<strong>Created:</strong>"));
}
