mod common;

use axum::http::{header, StatusCode};
use common::{get_admin_csrf, response_body_string, setup, with_proxy_token};
use shortener::db::links::{KindFilter, LinkSort, ListParams, StatusFilter};
use shortener::state::now_millis;
use tower::ServiceExt;

fn params(status: StatusFilter, sort: LinkSort) -> ListParams {
    ListParams {
        query: None,
        status,
        kind: KindFilter::All,
        sort,
        page: 1,
        per_page: 20,
        now: now_millis(),
    }
}

async fn make_links(app: &common::TestApp) {
    let db = &app.state.db;
    // alpha: plain active. beta: active with future expiry.
    common::create_link(&app.state, Some("alpha"), "https://example.com/a", None).await;
    let future = now_millis() + 3_600_000;
    common::create_link(
        &app.state,
        Some("beta"),
        "https://example.com/b",
        Some(future),
    )
    .await;
    // old: enabled but expired (backdated past the create-time check).
    common::create_link(
        &app.state,
        Some("old"),
        "https://example.com/c",
        Some(future),
    )
    .await;
    sqlx::query("UPDATE links SET expires_at = ? WHERE slug = 'old'")
        .bind(now_millis() - 1_000)
        .execute(db)
        .await
        .unwrap();
    // off: disabled. offexp: disabled AND expired (disabled wins).
    common::create_link(&app.state, Some("off"), "https://example.com/d", None).await;
    shortener::db::links::set_disabled(db, "off", true)
        .await
        .unwrap();
    common::create_link(
        &app.state,
        Some("offexp"),
        "https://example.com/e",
        Some(future),
    )
    .await;
    sqlx::query("UPDATE links SET expires_at = ? WHERE slug = 'offexp'")
        .bind(now_millis() - 1_000)
        .execute(db)
        .await
        .unwrap();
    shortener::db::links::set_disabled(db, "offexp", true)
        .await
        .unwrap();
}

fn slugs(items: &[shortener::db::links::LinkListItem]) -> Vec<&str> {
    items.iter().map(|i| i.link.slug.as_str()).collect()
}

#[tokio::test]
async fn status_filters_partition_links() {
    let app = setup().await;
    make_links(&app).await;

    let all = shortener::db::links::list_links(
        &app.state.db,
        params(StatusFilter::All, LinkSort::SlugAsc),
    )
    .await
    .unwrap();
    assert_eq!(all.total, 5);

    let active = shortener::db::links::list_links(
        &app.state.db,
        params(StatusFilter::Active, LinkSort::SlugAsc),
    )
    .await
    .unwrap();
    assert_eq!(slugs(&active.items), vec!["alpha", "beta"]);

    let expired = shortener::db::links::list_links(
        &app.state.db,
        params(StatusFilter::Expired, LinkSort::SlugAsc),
    )
    .await
    .unwrap();
    assert_eq!(slugs(&expired.items), vec!["old"]);

    // Disabled takes precedence: offexp is expired too but only disabled lists it.
    let disabled = shortener::db::links::list_links(
        &app.state.db,
        params(StatusFilter::Disabled, LinkSort::SlugAsc),
    )
    .await
    .unwrap();
    assert_eq!(slugs(&disabled.items), vec!["off", "offexp"]);
}

#[tokio::test]
async fn expiring_soon_boundaries_compose_with_search_kind_and_pagination() {
    use shortener::db::links::{list_links, EXPIRING_SOON_MILLIS};
    let app = setup().await;
    let now = now_millis();
    for (slug, expiry, disabled, text) in [
        ("soon-first", Some(now + 1), false, false),
        ("soon-last", Some(now + EXPIRING_SOON_MILLIS), false, false),
        ("soon-text", Some(now + 1000), false, true),
        ("soon-expired", Some(now), false, false),
        (
            "soon-later",
            Some(now + EXPIRING_SOON_MILLIS + 1),
            false,
            false,
        ),
        ("soon-never", None, false, false),
        ("soon-disabled", Some(now + 1000), true, false),
    ] {
        common::create_link(&app.state, Some(slug), "https://example.com", None).await;
        sqlx::query(
            "UPDATE links SET expires_at = ?, disabled_at = ?, text_content = ?, target_url = ? WHERE slug = ?",
        )
        .bind(expiry)
        .bind(disabled.then_some(now))
        .bind(text.then_some("notes"))
        .bind(if text { "" } else { "https://example.com/" })
        .bind(slug)
        .execute(&app.state.db)
        .await
        .unwrap();
    }
    let mut query = params(StatusFilter::ExpiringSoon, LinkSort::SlugAsc);
    query.now = now;
    let result = list_links(&app.state.db, query).await.unwrap();
    assert_eq!(result.total, 3);
    assert_eq!(
        slugs(&result.items),
        vec!["soon-first", "soon-last", "soon-text"]
    );
    for (kind, expected, total) in [
        (KindFilter::Url, "soon-last", 2),
        (KindFilter::Text, "soon-text", 1),
    ] {
        let mut query = params(StatusFilter::ExpiringSoon, LinkSort::SlugAsc);
        query.now = now;
        query.query = Some("soon-".into());
        query.kind = kind;
        query.per_page = 1;
        query.page = 2;
        let result = list_links(&app.state.db, query).await.unwrap();
        assert_eq!(result.total, total);
        assert_eq!(slugs(&result.items), vec![expected]);
    }
}

#[tokio::test]
async fn expiring_soon_is_available_in_admin_and_api() {
    let app = setup().await;
    make_links(&app).await;
    for path in [
        "/admin?status=expiring&kind=url",
        "/api/v1/links?status=expiring&kind=url",
    ] {
        let req = with_proxy_token(axum::http::Request::builder().uri(path))
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, _, body) =
            response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if path.starts_with("/api") {
            let data: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(data["data"].as_array().unwrap().len(), 1);
            assert_eq!(data["data"][0]["slug"], "beta");
        } else {
            assert!(body.contains("value=\"expiring\" selected"));
            assert!(body.contains("/admin/links/beta"));
            assert!(!body.contains("/admin/links/alpha"));
            assert!(body.contains("status=expiring&amp;kind=url"));
        }
    }
}

#[tokio::test]
async fn sort_modes_order_deterministically() {
    let app = setup().await;
    make_links(&app).await;
    let db = &app.state.db;

    // Pin timestamps so ordering never depends on test timing.
    sqlx::query(
        "UPDATE links SET
            created_at = CASE slug
                WHEN 'alpha' THEN 1000 WHEN 'beta' THEN 2000 WHEN 'old' THEN 3000
                WHEN 'off' THEN 4000 ELSE 5000 END,
            updated_at = CASE slug
                WHEN 'alpha' THEN 1000 WHEN 'beta' THEN 3000 WHEN 'old' THEN 2000
                WHEN 'off' THEN 4000 ELSE 5000 END",
    )
    .execute(db)
    .await
    .unwrap();

    let newest = shortener::db::links::list_links(db, params(StatusFilter::All, LinkSort::Newest))
        .await
        .unwrap();
    assert_eq!(newest.items[0].link.slug, "offexp");
    assert_eq!(newest.items[4].link.slug, "alpha");

    let oldest = shortener::db::links::list_links(db, params(StatusFilter::All, LinkSort::Oldest))
        .await
        .unwrap();
    assert_eq!(oldest.items[0].link.slug, "alpha");

    let updated =
        shortener::db::links::list_links(db, params(StatusFilter::All, LinkSort::RecentlyUpdated))
            .await
            .unwrap();
    assert_eq!(updated.items[0].link.slug, "offexp");
    assert_eq!(updated.items[1].link.slug, "off");

    let by_slug =
        shortener::db::links::list_links(db, params(StatusFilter::All, LinkSort::SlugAsc))
            .await
            .unwrap();
    let names = slugs(&by_slug.items);
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);

    // Click ranking with a stable slug tiebreak for the zero-click majority.
    let beta_id = shortener::db::links::get_link(db, "beta").await.unwrap().id;
    let alpha_id = shortener::db::links::get_link(db, "alpha")
        .await
        .unwrap()
        .id;
    let now = now_millis();
    for _ in 0..3 {
        shortener::db::analytics::record_click(db, &beta_id, now)
            .await
            .unwrap();
    }
    shortener::db::analytics::record_click(db, &alpha_id, now)
        .await
        .unwrap();

    let clicked =
        shortener::db::links::list_links(db, params(StatusFilter::All, LinkSort::MostClicked))
            .await
            .unwrap();
    assert_eq!(clicked.items[0].link.slug, "beta");
    assert_eq!(clicked.items[0].total_clicks, 3);
    assert_eq!(clicked.items[1].link.slug, "alpha");
    assert_eq!(clicked.items[1].total_clicks, 1);
    // Remaining zero-click links stay in slug order behind the ranked ones.
    let tail = slugs(&clicked.items[2..]);
    let mut tail_sorted = tail.clone();
    tail_sorted.sort_unstable();
    assert_eq!(tail, tail_sorted);
    assert!(clicked.items[2..].iter().all(|i| i.total_clicks == 0));
}

#[tokio::test]
async fn pagination_clamps_past_the_last_page() {
    let app = setup().await;
    common::create_link(&app.state, Some("solo"), "https://example.com/solo", None).await;

    let result = shortener::db::links::list_links(
        &app.state.db,
        ListParams {
            query: None,
            status: StatusFilter::All,
            kind: KindFilter::All,
            sort: LinkSort::Newest,
            page: 99,
            per_page: 20,
            now: now_millis(),
        },
    )
    .await
    .unwrap();
    assert_eq!(result.page, 1);
    assert_eq!(result.total, 1);
    assert_eq!(result.items.len(), 1);
}

#[tokio::test]
async fn search_combines_with_status_filter() {
    let app = setup().await;
    make_links(&app).await;

    let result = shortener::db::links::list_links(
        &app.state.db,
        ListParams {
            query: Some("example.com/a".to_string()),
            status: StatusFilter::Active,
            kind: KindFilter::All,
            sort: LinkSort::Newest,
            page: 1,
            per_page: 20,
            now: now_millis(),
        },
    )
    .await
    .unwrap();
    assert_eq!(slugs(&result.items), vec!["alpha"]);
}

#[tokio::test]
async fn invalid_filter_values_fall_back_on_admin_pages() {
    let app = setup().await;
    common::create_link(&app.state, Some("fallback"), "https://example.com/fb", None).await;
    let req =
        with_proxy_token(axum::http::Request::builder().uri("/admin?status=bogus&sort=bogus"))
            .body(axum::body::Body::empty())
            .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("/fb"));
}

#[tokio::test]
async fn admin_defaults_to_active_and_all_remains_available() {
    let app = setup().await;
    make_links(&app).await;
    for uri in ["/admin", "/admin?status=", "/admin?status=bogus"] {
        let req = with_proxy_token(axum::http::Request::builder().uri(uri))
            .body(axum::body::Body::empty())
            .unwrap();
        let (status, _, html) =
            response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("value=\"active\" selected"));
        for slug in ["alpha", "beta"] {
            assert!(
                html.contains(&format!("/admin/links/{slug}\"")),
                "{uri}: missing {slug}"
            );
        }
        for slug in ["old", "off", "offexp"] {
            assert!(
                !html.contains(&format!("/admin/links/{slug}\"")),
                "{uri}: unexpected {slug}"
            );
        }
    }
    let req = with_proxy_token(axum::http::Request::builder().uri("/admin?status=all"))
        .body(axum::body::Body::empty())
        .unwrap();
    let (_, _, html) = response_body_string(app.router.oneshot(req).await.unwrap()).await;
    assert!(html.contains("value=\"all\" selected"));
    for slug in ["alpha", "beta", "old", "off", "offexp"] {
        assert!(html.contains(&format!("/admin/links/{slug}\"")));
    }
}

async fn create_text(app: &common::TestApp, slug: &str) {
    shortener::db::links::create_link(
        &app.state.db,
        &app.state.config.base_url,
        shortener::domain::link::CreateLinkInput {
            text_content: Some("Shared notes".into()),
            target_url: String::new(),
            custom_slug: Some(slug.into()),
            label: None,
            expires_at: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn type_filters_combine_with_status_search_and_pagination() {
    let app = setup().await;
    make_links(&app).await;
    for i in 0..25 {
        create_text(&app, &format!("notes-{i:02}")).await;
    }
    create_text(&app, "notes-disabled").await;
    shortener::db::links::set_disabled(&app.state.db, "notes-disabled", true)
        .await
        .unwrap();
    create_text(&app, "unrelated").await;
    common::create_link(
        &app.state,
        Some("notes-url"),
        "https://example.com/notes",
        None,
    )
    .await;

    let query = "status=active&kind=text&q=notes&sort=slug&per_page=20&page=2";
    let req = with_proxy_token(axum::http::Request::builder().uri(format!("/admin?{query}")))
        .body(axum::body::Body::empty())
        .unwrap();
    let (_, _, html) = response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
    assert!(html.contains("value=\"text\" selected"));
    assert!(html.contains("items 21–25 of 25"));
    assert!(html.contains("Page 2 of 2"));
    assert!(html.contains("kind=text"));
    assert!(html.contains("page=1"));
    assert!(!html.contains("/admin/links/notes-url\""));
    assert!(!html.contains("/admin/links/notes-disabled\""));

    for (query, total, shown) in [
        (query, 25, 5),
        ("kind=url", 6, 6),
        ("kind=text&status=disabled", 1, 1),
        ("", 33, 20),
    ] {
        let req =
            with_proxy_token(axum::http::Request::builder().uri(format!("/api/v1/links?{query}")))
                .body(axum::body::Body::empty())
                .unwrap();
        let (status, _, body) =
            response_body_string(app.router.clone().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::OK);
        let result: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(result["total"], total, "{query}");
        assert_eq!(result["data"].as_array().unwrap().len(), shown, "{query}");
    }
    let req = with_proxy_token(axum::http::Request::builder().uri("/api/v1/links?kind=unknown"))
        .body(axum::body::Body::empty())
        .unwrap();
    assert_eq!(
        app.router.oneshot(req).await.unwrap().status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn navigation_preserves_view_state() {
    let app = setup().await;
    for i in 0..25 {
        common::create_link(
            &app.state,
            Some(&format!("nav{i:02}")),
            "https://example.com/nav",
            None,
        )
        .await;
    }
    let req = with_proxy_token(
        axum::http::Request::builder()
            .uri("/admin?status=all&kind=url&q=nav&sort=slug&per_page=20"),
    )
    .body(axum::body::Body::empty())
    .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    // Next-page link carries the full view state (Askama escapes & as &amp;).
    assert!(body.contains("page=2"), "next link missing: {body}");
    assert!(body.contains("sort=slug"), "sort not preserved");
    assert!(body.contains("status=all"), "status not preserved");
    assert!(body.contains("kind=url"), "type not preserved");
    assert!(body.contains("per_page=20"), "page size not preserved");
    assert!(body.contains("of 25"), "range total missing");
    assert!(body.contains("Page 1 of 2"), "page indicator missing");
    // The transient created banner parameter is never part of navigation.
    assert!(!body.contains("created="), "created leaked into nav");

    // The filter form itself carries no page field, so applying filters
    // always returns to page 1.
    assert!(
        !body.contains("name=\"page\""),
        "filter form must not pin a page"
    );
}

#[tokio::test]
async fn out_of_range_page_clamps_in_html() {
    let app = setup().await;
    common::create_link(&app.state, Some("clamped"), "https://example.com/c", None).await;
    let req = with_proxy_token(axum::http::Request::builder().uri("/admin?page=99"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = app.router.clone().oneshot(req).await.unwrap();
    let (status, _, body) = response_body_string(res).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("/clamped"),
        "clamped page should show content"
    );
}

#[tokio::test]
async fn inline_toggle_returns_to_list_view() {
    let app = setup().await;
    common::create_link(&app.state, Some("toggle"), "https://example.com/t", None).await;
    let (csrf, cookie) = get_admin_csrf(&app).await;

    let post_toggle = |return_to: Option<&str>| {
        let mut form = String::from("csrf_token=");
        form.push_str(&csrf);
        if let Some(r) = return_to {
            form.push_str("&return_to=");
            form.push_str(&url_encode(r));
        }
        with_proxy_token(
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/links/toggle/disable"),
        )
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("csrf_token={cookie}"))
        .header(header::ORIGIN, "http://localhost")
        .body(axum::body::Body::from(form))
        .unwrap()
    };

    // Valid list-view target is preserved with a 303.
    let res = app
        .router
        .clone()
        .oneshot(post_toggle(Some(
            "/admin?status=disabled&kind=url&per_page=50",
        )))
        .await
        .unwrap();
    let (status, headers, _) = response_body_string(res).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get(header::LOCATION).unwrap(),
        "/admin?status=disabled&kind=url&per_page=50"
    );

    // Absolute, protocol-relative, and off-admin targets fall back safely.
    for evil in [
        "https://evil.example/x",
        "//evil.example/x",
        "/other/path",
        "/adminevil",
    ] {
        // Re-enable first so disable works again.
        let enable = with_proxy_token(
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/links/toggle/enable"),
        )
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::COOKIE, format!("csrf_token={cookie}"))
        .header(header::ORIGIN, "http://localhost")
        .body(axum::body::Body::from(format!("csrf_token={csrf}")))
        .unwrap();
        let res = app.router.clone().oneshot(enable).await.unwrap();
        let (status, _, _) = response_body_string(res).await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        let res = app
            .router
            .clone()
            .oneshot(post_toggle(Some(evil)))
            .await
            .unwrap();
        let (status, headers, _) = response_body_string(res).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(
            headers.get(header::LOCATION).unwrap(),
            "/admin/links/toggle",
            "unsafe return_to {evil:?} must fall back"
        );
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
