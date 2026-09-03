mod common;

use axum::http::StatusCode;
use common::setup;
use shortener::db::analytics::{
    day_start_utc, get_link_activity, link_stats, record_click, MILLIS_PER_DAY,
};
use tower::ServiceExt;

/// 2024-01-01T00:00:00Z, a UTC midnight used to pin day-boundary tests.
const MIDNIGHT: i64 = 1_704_067_200_000;

async fn test_link(state: &shortener::state::AppState, slug: &str) -> String {
    common::create_link(state, Some(slug), "https://example.com/analytics", None).await;
    let link = shortener::db::links::get_link(&state.db, slug)
        .await
        .unwrap();
    link.id
}

#[tokio::test]
async fn same_day_clicks_share_one_aggregate_row() {
    let app = setup().await;
    let id = test_link(&app.state, "sameday").await;

    for offset in [0, 1_000, 3_600_000] {
        record_click(&app.state.db, &id, MIDNIGHT + offset)
            .await
            .unwrap();
    }

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM link_daily_clicks WHERE link_id = ?")
        .bind(&id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    let (total, last) = link_stats(&app.state.db, &id).await.unwrap();
    assert_eq!(total, 3);
    assert_eq!(last, Some(MIDNIGHT + 3_600_000));
}

#[tokio::test]
async fn clicks_across_utc_midnight_split_into_two_rows() {
    let app = setup().await;
    let id = test_link(&app.state, "boundary").await;

    record_click(&app.state.db, &id, MIDNIGHT - 1)
        .await
        .unwrap();
    record_click(&app.state.db, &id, MIDNIGHT).await.unwrap();

    let days: Vec<i64> = sqlx::query_scalar(
        "SELECT day_start_utc FROM link_daily_clicks WHERE link_id = ? ORDER BY day_start_utc",
    )
    .bind(&id)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(days, vec![MIDNIGHT - MILLIS_PER_DAY, MIDNIGHT]);
    assert_eq!(day_start_utc(MIDNIGHT - 1), MIDNIGHT - MILLIS_PER_DAY);
    assert_eq!(day_start_utc(MIDNIGHT), MIDNIGHT);
}

#[tokio::test]
async fn activity_series_is_complete_and_zero_filled() {
    let app = setup().await;
    let id = test_link(&app.state, "series").await;

    // Clicks 29 days ago, yesterday (twice), and an hour ago today.
    let now = MIDNIGHT + 29 * MILLIS_PER_DAY + 3_600_000;
    record_click(&app.state.db, &id, MIDNIGHT).await.unwrap();
    record_click(&app.state.db, &id, MIDNIGHT + 28 * MILLIS_PER_DAY)
        .await
        .unwrap();
    record_click(&app.state.db, &id, MIDNIGHT + 28 * MILLIS_PER_DAY + 5_000)
        .await
        .unwrap();
    record_click(&app.state.db, &id, now).await.unwrap();

    let summary = get_link_activity(&app.state.db, &id, 30, now)
        .await
        .unwrap();
    assert_eq!(summary.total_clicks, 4);
    assert_eq!(summary.last_clicked_at, Some(now));
    // Last seven calendar days hold yesterday's two plus today's one.
    assert_eq!(summary.last_7_days_clicks, 3);

    assert_eq!(summary.daily.len(), 30);
    for window in summary.daily.windows(2) {
        assert_eq!(
            window[1].day_start_utc - window[0].day_start_utc,
            MILLIS_PER_DAY
        );
    }
    assert_eq!(summary.daily[0].day_start_utc, MIDNIGHT);
    assert_eq!(summary.daily[0].click_count, 1);
    assert_eq!(summary.daily[28].click_count, 2);
    assert_eq!(summary.daily[29].click_count, 1);
    assert!(summary.daily[1..28].iter().all(|d| d.click_count == 0));

    // A link with no clicks reports an honest empty series.
    let quiet = test_link(&app.state, "quiet").await;
    let empty = get_link_activity(&app.state.db, &quiet, 30, now)
        .await
        .unwrap();
    assert_eq!(empty.total_clicks, 0);
    assert_eq!(empty.last_7_days_clicks, 0);
    assert_eq!(empty.last_clicked_at, None);
    assert_eq!(empty.daily.len(), 30);
    assert!(empty.daily.iter().all(|d| d.click_count == 0));
}

async fn get_status(
    app: &common::TestApp,
    method: &str,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let req = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    common::response_body_string(res).await
}

async fn total_clicks(app: &common::TestApp, slug: &str) -> i64 {
    let link = shortener::db::links::get_link(&app.state.db, slug)
        .await
        .unwrap();
    link_stats(&app.state.db, &link.id).await.unwrap().0
}

#[tokio::test]
async fn two_get_redirects_increment_twice() {
    let app = setup().await;
    common::create_link(
        &app.state,
        Some("counted"),
        "https://example.com/counted",
        None,
    )
    .await;

    for _ in 0..2 {
        let (status, _, _) = get_status(&app, "GET", "/counted").await;
        assert_eq!(status, StatusCode::FOUND);
    }
    assert_eq!(total_clicks(&app, "counted").await, 2);
}

#[tokio::test]
async fn head_missing_disabled_and_expired_produce_no_click() {
    let app = setup().await;
    common::create_link(&app.state, Some("nohead"), "https://example.com/x", None).await;
    common::create_link(&app.state, Some("off"), "https://example.com/x", None).await;
    shortener::db::links::set_disabled(&app.state.db, "off", true)
        .await
        .unwrap();

    // HEAD answers like GET but is never counted (health checks stay clean).
    let (status, _, _) = get_status(&app, "HEAD", "/nohead").await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(total_clicks(&app, "nohead").await, 0);

    // Unknown slugs 404 with nothing to attribute.
    let (status, _, _) = get_status(&app, "GET", "/definitely-missing").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Disabled links 404 and expired links 410, both uncounted.
    let (status, _, _) = get_status(&app, "GET", "/off").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(total_clicks(&app, "off").await, 0);

    let future = shortener::state::now_millis() + 60_000;
    common::create_link(
        &app.state,
        Some("stale"),
        "https://example.com/x",
        Some(future),
    )
    .await;
    // Backdate past the create-time future check so the link reads expired.
    let past = shortener::state::now_millis() - 1_000;
    sqlx::query("UPDATE links SET expires_at = ? WHERE slug = ?")
        .bind(past)
        .bind("stale")
        .execute(&app.state.db)
        .await
        .unwrap();
    let (status, _, _) = get_status(&app, "GET", "/stale").await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(total_clicks(&app, "stale").await, 0);
}

#[tokio::test]
async fn analytics_failure_leaves_redirects_functional() {
    let app = setup().await;
    common::create_link(
        &app.state,
        Some("resilient"),
        "https://example.com/resilient",
        None,
    )
    .await;

    // Simulate a broken analytics store: the links table (and its rows)
    // survive, but recording must fail.
    sqlx::query("DROP TABLE link_daily_clicks")
        .execute(&app.state.db)
        .await
        .unwrap();

    let (status, headers, _) = get_status(&app, "GET", "/resilient").await;
    assert_eq!(status, StatusCode::FOUND);
    assert_eq!(
        headers.get(axum::http::header::LOCATION).unwrap(),
        "https://example.com/resilient"
    );
}

async fn get_edit_page(app: &common::TestApp, slug: &str) -> (StatusCode, String) {
    use tower::ServiceExt;
    let req = common::with_proxy_token(
        axum::http::Request::builder().uri(format!("/admin/links/{slug}")),
    )
    .body(axum::body::Body::empty())
    .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, body) = common::response_body_string(res).await;
    (status, body)
}

fn count_bars(html: &str) -> usize {
    html.matches("class=\"bar\"").count()
}

#[tokio::test]
async fn activity_card_shows_empty_state() {
    let app = setup().await;
    common::create_link(&app.state, Some("cardempty"), "https://example.com/e", None).await;

    let (status, body) = get_edit_page(&app, "cardempty").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Activity"), "activity card missing");
    assert!(body.contains("Never"), "unclicked link should show Never");
    assert!(body.contains("0</strong> total clicks"), "body: {body}");
    assert_eq!(count_bars(&body), 30, "chart always renders 30 bars");
}

#[tokio::test]
async fn activity_card_shows_populated_state() {
    let app = setup().await;
    common::create_link(&app.state, Some("cardfull"), "https://example.com/f", None).await;

    // Three real redirects land in today's UTC bucket together.
    for _ in 0..3 {
        let (status, _, _) = get_status(&app, "GET", "/cardfull").await;
        assert_eq!(status, StatusCode::FOUND);
    }

    let (status, body) = get_edit_page(&app, "cardfull").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("3</strong> total clicks"), "body: {body}");
    assert_eq!(count_bars(&body), 30);
    // Today's bar carries an accessible per-day label; as the only nonzero
    // bucket it also renders at full height.
    assert!(body.contains(": 3 clicks"), "today's bar label missing");
    assert!(body.contains("--h: 100%"), "full-height bar missing");
}

#[tokio::test]
async fn api_link_responses_carry_click_fields() {
    use tower::ServiceExt;
    let app = setup().await;

    let api = |method: &str, uri: &str, json: Option<String>| {
        let mut b =
            common::with_proxy_token(axum::http::Request::builder().method(method).uri(uri));
        if json.is_some() {
            b = b.header(axum::http::header::CONTENT_TYPE, "application/json");
        }
        b = b.header("X-Requested-With", "XMLHttpRequest");
        let body = json.unwrap_or_default();
        b.body(axum::body::Body::from(body)).unwrap()
    };

    // Create: fresh link reports zeros.
    let res = app
        .router
        .clone()
        .oneshot(api(
            "POST",
            "/api/v1/links",
            Some(
                r#"{"target_url":"https://example.com/apiclicks","custom_slug":"apiclicks"}"#
                    .to_string(),
            ),
        ))
        .await
        .unwrap();
    let (status, _, body) = common::response_body_string(res).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["total_clicks"], 0);
    assert!(v["last_clicked_at"].is_null(), "{body}");

    // Two public GETs flow into the API view.
    for _ in 0..2 {
        let (status, _, _) = get_status(&app, "GET", "/apiclicks").await;
        assert_eq!(status, StatusCode::FOUND);
    }
    let res = app
        .router
        .clone()
        .oneshot(api("GET", "/api/v1/links/apiclicks", None))
        .await
        .unwrap();
    let (status, _, body) = common::response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["total_clicks"], 2);
    assert!(v["last_clicked_at"].is_string(), "{body}");

    // List supports status/sort and carries the fields on every item.
    let res = app
        .router
        .clone()
        .oneshot(api("GET", "/api/v1/links?status=active&sort=clicked", None))
        .await
        .unwrap();
    let (status, _, body) = common::response_body_string(res).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let items = v["data"].as_array().unwrap();
    assert!(items.iter().any(|i| i["slug"] == "apiclicks"));
    assert!(items.iter().all(|i| i.get("total_clicks").is_some()));
    assert!(items.iter().all(|i| i.get("last_clicked_at").is_some()));

    // Invalid values use the existing JSON validation-error shape.
    for uri in ["/api/v1/links?status=bogus", "/api/v1/links?sort=bogus"] {
        let res = app
            .router
            .clone()
            .oneshot(api("GET", uri, None))
            .await
            .unwrap();
        let (status, _, body) = common::response_body_string(res).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{uri}: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(v["error"]["message"].is_string(), "{body}");
    }
}
