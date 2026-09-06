mod common;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use common::{response_body_string, setup, with_proxy_token, TestApp};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn api(app: &TestApp, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let req = with_proxy_token(Request::builder().method(method).uri(path))
        .header(header::CONTENT_TYPE, "application/json")
        .header("X-Requested-With", "XMLHttpRequest")
        .body(Body::from(body.to_string()))
        .unwrap();
    let (status, _, text) =
        response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

async fn public(
    app: &TestApp,
    method: &str,
    path: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .body(Body::empty())
        .unwrap();
    response_body_string(app.router.clone().oneshot(req).await.unwrap()).await
}

#[tokio::test]
async fn shared_text_lifecycle_and_public_write_protection() {
    let app = setup().await;
    let text = "\n  Hello 🦀\n\t</textarea><script>alert('x')</script> & goodbye\n";
    let (status, created) = api(
        &app,
        "POST",
        "/api/v1/links",
        json!({"text_content":text,"custom_slug":"Notes","label":"Team notes"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["slug"], "notes");
    assert_eq!(created["text_content"], text);
    assert_eq!(created["target_url"], "");
    let (status, headers, html) = public(&app, "GET", "/NOTES").await;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key(header::LOCATION));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers["x-robots-tag"], "noindex, nofollow");
    assert!(headers[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap()
        .contains("form-action 'none'"));
    assert!(html.contains("readonly") && html.contains("Copy text"));
    assert!(html.contains("Team notes"));
    assert!(!html.contains("<script>alert") && !html.contains("<form"));
    assert!(html.contains("&lt;") && html.contains("&amp;"));
    let (status, _, body) = public(&app, "HEAD", "/notes").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        assert_eq!(
            public(&app, method, "/notes").await.0,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }
    for (method, path) in [
        ("POST", "/admin/links/notes"),
        ("PATCH", "/api/v1/links/notes"),
        ("POST", "/api/v1/links"),
        ("GET", "/admin/links/notes"),
    ] {
        assert_eq!(public(&app, method, path).await.0, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        api(&app, "GET", "/api/v1/links/notes", json!(null)).await.1["text_content"],
        text
    );
    let (status, updated) = api(
        &app,
        "PATCH",
        "/api/v1/links/notes",
        json!({"text_content":"Updated text"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert!(public(&app, "GET", "/notes")
        .await
        .2
        .contains("Updated text"));
    let (_, listed) = api(&app, "GET", "/api/v1/links?q=notes", json!(null)).await;
    assert_eq!(listed["data"][0]["text_content"], "Updated text");
    assert_eq!(
        api(&app, "DELETE", "/api/v1/links/notes", json!(null))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(public(&app, "GET", "/notes").await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        api(&app, "POST", "/api/v1/links/notes/enable", json!(null))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(public(&app, "GET", "/notes").await.0, StatusCode::OK);
    sqlx::query("UPDATE links SET expires_at = 1 WHERE slug = 'notes'")
        .execute(&app.state.db)
        .await
        .unwrap();
    assert_eq!(public(&app, "GET", "/notes").await.0, StatusCode::GONE);
}

#[tokio::test]
async fn text_validation_slug_conflicts_and_type_safety() {
    let app = setup().await;
    for body in [
        json!({}),
        json!({"text_content":" \n\t"}),
        json!({"text_content":"bad\u{0000}text"}),
        json!({"text_content":"é".repeat(32769)}),
        json!({"text_content":"notes","target_url":"https://example.com"}),
        json!({"text_content":"notes","custom_slug":"admin"}),
        json!({"text_content":"notes","expires_at":1}),
    ] {
        assert_eq!(
            api(&app, "POST", "/api/v1/links", body).await.0,
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
    // Worst-case JSON escaping stays below the request cap at the text limit.
    let text = format!("a{}", "\t".repeat(65535));
    let (status, created) = api(&app, "POST", "/api/v1/links", json!({"text_content":text})).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["text_content"], text);
    assert_eq!(created["slug"].as_str().unwrap().len(), 10);
    common::create_link(&app.state, Some("taken"), "https://example.com", None).await;
    assert_eq!(
        api(
            &app,
            "POST",
            "/api/v1/links",
            json!({"text_content":"notes","custom_slug":"taken"})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        api(
            &app,
            "PATCH",
            "/api/v1/links/taken",
            json!({"text_content":"notes"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let path = format!("/api/v1/links/{}", created["slug"].as_str().unwrap());
    assert_eq!(
        api(
            &app,
            "PATCH",
            &path,
            json!({"target_url":"https://example.com"})
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        api(&app, "PATCH", &path, json!({"text_content":""}))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

async fn form(app: &TestApp, path: &str, fields: &[(&str, &str)]) -> (StatusCode, String) {
    let (token, cookie) = common::get_admin_csrf(app).await;
    let mut fields = fields.to_vec();
    fields.push(("csrf_token", &token));
    let req = with_proxy_token(Request::builder().method("POST").uri(path))
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ORIGIN, "http://localhost")
        .header(header::COOKIE, format!("csrf_token={cookie}"))
        .body(Body::from(serde_urlencoded::to_string(fields).unwrap()))
        .unwrap();
    let (status, _, html) =
        response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
    (status, html)
}

#[tokio::test]
async fn admin_text_forms_create_edit_and_preserve_drafts() {
    let app = setup().await;
    let fields = [
        ("text_content", "Draft <notes>\nsecond line"),
        ("custom_slug", "adminnotes"),
        ("label", "Notes"),
    ];
    assert_eq!(
        form(&app, "/admin/links", &fields).await.0,
        StatusCode::SEE_OTHER
    );
    let (status, html) = form(&app, "/admin/links", &fields).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(html.contains("Draft &lt;notes&gt;\nsecond line"));
    assert!(html.contains("<details open>"));
    let req = with_proxy_token(Request::builder().uri("/admin/links/adminnotes"))
        .body(Body::empty())
        .unwrap();
    let (_, _, html) = response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
    assert!(html.contains("name=\"text_content\""));
    assert!(!html.contains("name=\"target_url\""));
    let (status, html) = form(
        &app,
        "/admin/links/adminnotes",
        &[("text_content", "Unsaved draft"), ("expires_at", "invalid")],
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(html.contains("Unsaved draft"));
    assert_eq!(
        form(
            &app,
            "/admin/links/adminnotes",
            &[("text_content", "Saved text")]
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert!(public(&app, "GET", "/adminnotes")
        .await
        .2
        .contains("Saved text"));
    // Browser form encoding expands this boundary-sized input by 3x.
    let text = "é".repeat(32768);
    assert_eq!(
        form(&app, "/admin/links", &[("text_content", &text)])
            .await
            .0,
        StatusCode::SEE_OTHER
    );
    // Authentication alone cannot bypass CSRF.
    let req = with_proxy_token(
        Request::builder()
            .method("POST")
            .uri("/admin/links/adminnotes"),
    )
    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
    .header(header::ORIGIN, "http://localhost")
    .body(Body::from("text_content=Tampered"))
    .unwrap();
    assert_eq!(
        app.router.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn migration_preserves_existing_redirects_and_analytics() {
    let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    sqlx::raw_sql(include_str!("../migrations/0001_create_links.sql"))
        .execute(&db)
        .await
        .unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/0002_create_link_daily_clicks.sql"
    ))
    .execute(&db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO links (id, slug, target_url, created_at, updated_at) VALUES ('old-id', 'old-link', 'https://example.com/', 1, 1)").execute(&db).await.unwrap();
    sqlx::query("INSERT INTO link_daily_clicks (link_id, day_start_utc, click_count, last_clicked_at) VALUES ('old-id', 0, 3, 1)").execute(&db).await.unwrap();
    sqlx::raw_sql(include_str!("../migrations/0003_add_shared_text.sql"))
        .execute(&db)
        .await
        .unwrap();
    let link = shortener::db::links::get_link(&db, "old-link")
        .await
        .unwrap();
    assert_eq!(link.target_url, "https://example.com/");
    assert!(link.text_content.is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT click_count FROM link_daily_clicks WHERE link_id = 'old-id'"
        )
        .fetch_one(&db)
        .await
        .unwrap(),
        3
    );
}
