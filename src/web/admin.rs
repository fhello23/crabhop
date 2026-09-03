use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::db::links::{
    create_link, get_link, list_links, normalize_pagination, set_disabled, update_link,
    LinkListItem, LinkSort, ListParams, StatusFilter,
};
use crate::domain::link::CreateLinkInput;
use crate::domain::link::UpdateLinkInput;
use crate::error::AppError;
use crate::state::{millis_to_rfc3339, now_millis, parse_expires_at, AppState};
use crate::web::security::{
    csrf_set_cookie_value, extract_cookie_token, generate_csrf_token, validate_csrf_pair,
    verify_origin, CSRF_FORM_FIELD,
};

// ---------------------------------------------------------------------------
// Templates (Askama auto-escapes all `{{ }}` interpolations)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "links.html")]
#[allow(dead_code)]
struct LinksTemplate {
    title: String,
    brand_host: String,
    csrf_token: String,
    created_slug: Option<String>,
    created_short_url: Option<String>,
    error: Option<String>,
    query: String,
    status: String,
    sort: String,
    links: Vec<LinkRow>,
    page: u32,
    per_page: u32,
    total: i64,
    total_pages: u32,
    range_start: i64,
    range_end: i64,
    prev_url: Option<String>,
    next_url: Option<String>,
    list_url: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct LinkRow {
    slug: String,
    short_url: String,
    target_url: String,
    label: Option<String>,
    total_clicks: i64,
    created_at: String,
    updated_at: String,
    expires_at: Option<String>,
    disabled: bool,
    expired: bool,
}

#[derive(Template)]
#[template(path = "edit_link.html")]
struct EditTemplate {
    title: String,
    brand_host: String,
    csrf_token: String,
    slug: String,
    short_url: String,
    target_url: String,
    label: String,
    expires_input: String,
    expires_display: String,
    created_at: String,
    updated_at: String,
    disabled: bool,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Query / form types
// ---------------------------------------------------------------------------

/// Typed admin list parameters. Unknown status/sort values fall back to the
/// defaults (the JSON API rejects them instead — see `api.rs`).
#[derive(Debug, Deserialize, Default)]
pub struct AdminListQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    per_page: Option<i64>,
    #[serde(default)]
    created: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateForm {
    pub target_url: Option<String>,
    pub custom_slug: Option<String>,
    pub label: Option<String>,
    pub expires_at: Option<String>,
    pub csrf_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateForm {
    pub target_url: Option<String>,
    pub label: Option<String>,
    pub expires_at: Option<String>,
    pub csrf_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EmptyForm {
    pub csrf_token: Option<String>,
    /// Where to return after an inline enable/disable. Only same-origin
    /// `/admin` URLs are honored; anything else falls back to the detail page.
    #[serde(default)]
    pub return_to: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_secure_base(state: &AppState) -> bool {
    state.config.base_url.scheme() == "https"
}

/// Ensure a CSRF cookie/token pair for GET pages. Returns the token to embed
/// in forms plus an optional Set-Cookie value when issuance is needed.
fn ensure_csrf_token(headers: &HeaderMap, state: &AppState) -> (String, Option<String>) {
    let now = now_millis();
    if let Some(existing) = extract_cookie_token(headers, crate::web::security::CSRF_COOKIE_NAME) {
        if crate::web::security::verify_csrf_token(&state.config.csrf_signing_key, &existing, now) {
            return (existing, None);
        }
    }
    let token = generate_csrf_token(&state.config.csrf_signing_key, now);
    let cookie = csrf_set_cookie_value(&token, is_secure_base(state));
    (token, Some(cookie))
}

fn check_browser_mutation(
    headers: &HeaderMap,
    state: &AppState,
    form_token: Option<&str>,
) -> Result<(), AppError> {
    if !verify_origin(headers, &state.config.base_origin) {
        return Err(AppError::Forbidden(
            "missing or invalid Origin/Referer".to_string(),
        ));
    }
    let cookie_token = extract_cookie_token(headers, crate::web::security::CSRF_COOKIE_NAME);
    if !validate_csrf_pair(
        &state.config.csrf_signing_key,
        cookie_token.as_deref(),
        form_token,
        now_millis(),
    ) {
        return Err(AppError::Forbidden("invalid CSRF token".to_string()));
    }
    Ok(())
}

fn html_error(status: StatusCode, message: &str) -> Response {
    // Escape is handled by construction (no raw interpolation of `message`
    // without escaping — build via Askama-less manual escape).
    let safe = html_escape(message);
    let body = format!(
        "<!doctype html><html><head><title>{0}</title>\
         <link rel=\"stylesheet\" href=\"/static/app.css\"></head>\
         <body><main class=\"container\"><h1>{0}</h1><p>{safe}</p>\
         <p><a href=\"/admin\">Back to admin</a></p></main></body></html>",
        status.as_u16()
    );
    (status, Html(body)).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Display host for the admin UI chrome, derived from BASE_URL at runtime
/// so no domain is hardcoded in templates.
fn brand_host(state: &AppState) -> String {
    state
        .config
        .base_url
        .host_str()
        .unwrap_or("shortener")
        .to_string()
}

fn to_row(state: &AppState, item: LinkListItem, now: i64) -> LinkRow {
    LinkRow {
        short_url: state.short_url(&item.link.slug),
        slug: item.link.slug.clone(),
        target_url: item.link.target_url.clone(),
        label: item.link.label.clone(),
        total_clicks: item.total_clicks,
        created_at: millis_to_rfc3339(item.link.created_at),
        updated_at: millis_to_rfc3339(item.link.updated_at),
        expires_at: item.link.expires_at.map(millis_to_rfc3339),
        disabled: item.link.is_disabled(),
        expired: item.link.is_expired(now),
    }
}

/// Navigation parameters for the list view. The transient `created` banner
/// parameter is deliberately excluded so pagination never replays it.
#[derive(Serialize)]
struct ListNav<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    q: Option<&'a str>,
    status: &'a str,
    sort: &'a str,
    per_page: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<u32>,
}

/// List URL preserving search/filter/sort/page-size. The filter form itself
/// carries no `page`, so changing filters always returns to page 1.
fn list_url(
    q: &str,
    status: StatusFilter,
    sort: LinkSort,
    per_page: u32,
    page: Option<u32>,
) -> String {
    let nav = ListNav {
        q: if q.trim().is_empty() { None } else { Some(q) },
        status: status.as_param(),
        sort: sort.as_param(),
        per_page,
        page,
    };
    match serde_urlencoded::to_string(&nav) {
        Ok(s) if !s.is_empty() => format!("/admin?{s}"),
        _ => "/admin".to_string(),
    }
}

/// Accept only same-origin `/admin` return targets for inline toggles.
/// Anything else (absolute URLs, protocol-relative, controls) falls back to
/// the link detail page, so `return_to` can never become an open redirect.
fn safe_return_to(raw: Option<&str>, slug: &str) -> String {
    match raw {
        Some(r) => {
            let r = r.trim();
            let local_admin = r == "/admin" || r.starts_with("/admin/") || r.starts_with("/admin?");
            if local_admin && r.len() <= 512 && !r.chars().any(|c| c.is_control()) {
                return r.to_string();
            }
            format!("/admin/links/{slug}")
        }
        None => format!("/admin/links/{slug}"),
    }
}

/// Parse admin expiry input: empty => None; RFC3339 or millis; or
/// HTML datetime-local `YYYY-MM-DDTHH:MM` interpreted as UTC.
fn parse_admin_expires(raw: Option<&str>) -> Result<Option<i64>, AppError> {
    let s = raw.unwrap_or("").trim();
    if s.is_empty() {
        return Ok(None);
    }
    if let Some(m) = parse_expires_at(s) {
        return Ok(Some(m));
    }
    // datetime-local without seconds/timezone, e.g. 2026-12-31T23:59
    let fmt = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]T[hour]:[minute]")
        .map_err(AppError::internal)?;
    if let Ok(dt) = time::PrimitiveDateTime::parse(s, &fmt) {
        let odt = dt.assume_utc();
        return Ok(Some(odt.unix_timestamp() * 1000));
    }
    // With seconds: 2026-12-31T23:59:59
    let fmt2 = time::format_description::parse_borrowed::<2>(
        "[year]-[month]-[day]T[hour]:[minute]:[second]",
    )
    .map_err(AppError::internal)?;
    if let Ok(dt) = time::PrimitiveDateTime::parse(s, &fmt2) {
        let odt = dt.assume_utc();
        return Ok(Some(odt.unix_timestamp() * 1000));
    }
    Err(AppError::Validation(
        "expires_at must be empty or a valid date/time (RFC 3339 or YYYY-MM-DDTHH:MM)".to_string(),
    ))
}

fn format_datetime_local(millis: Option<i64>) -> String {
    match millis {
        None => String::new(),
        Some(m) => {
            let secs = m.div_euclid(1000);
            let dt = time::OffsetDateTime::from_unix_timestamp(secs)
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
            let fmt = time::format_description::parse_borrowed::<2>(
                "[year]-[month]-[day]T[hour]:[minute]",
            )
            .expect("valid format");
            dt.format(&fmt).unwrap_or_default()
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn admin_list_with_created(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<AdminListQuery>,
) -> Response {
    let (csrf_token, set_cookie) = ensure_csrf_token(&headers, &state);
    let q = params.q.clone().unwrap_or_default();
    let created_slug = params.created.clone();
    let status = StatusFilter::from_param(params.status.as_deref()).unwrap_or_default();
    let sort = LinkSort::from_param(params.sort.as_deref()).unwrap_or_default();
    let (page, per_page) = normalize_pagination(params.page, params.per_page);
    let now = now_millis();

    match list_links(
        &state.db,
        ListParams {
            query: if q.trim().is_empty() {
                None
            } else {
                Some(q.clone())
            },
            status,
            sort,
            page,
            per_page,
            now,
        },
    )
    .await
    {
        Ok(result) => {
            let rows: Vec<LinkRow> = result
                .items
                .into_iter()
                .map(|item| to_row(&state, item, now))
                .collect();
            let shown = rows.len() as i64;
            let total_pages = ((result.total as f64) / (per_page as f64)).ceil() as u32;
            let (range_start, range_end) = if result.total == 0 {
                (0, 0)
            } else {
                let start = ((result.page - 1) * result.per_page) as i64 + 1;
                (start, (start + shown - 1).min(result.total))
            };
            let tpl = LinksTemplate {
                title: "Admin — Links".to_string(),
                brand_host: brand_host(&state),
                csrf_token,
                created_short_url: created_slug.as_ref().map(|s| state.short_url(s)),
                created_slug,
                error: None,
                query: q.clone(),
                status: status.as_param().to_string(),
                sort: sort.as_param().to_string(),
                links: rows,
                page: result.page,
                per_page: result.per_page,
                total: result.total,
                total_pages: total_pages.max(1),
                range_start,
                range_end,
                prev_url: (result.page > 1)
                    .then(|| list_url(&q, status, sort, per_page, Some(result.page - 1))),
                next_url: (result.page < total_pages.max(1))
                    .then(|| list_url(&q, status, sort, per_page, Some(result.page + 1))),
                list_url: list_url(&q, status, sort, per_page, Some(result.page)),
            };
            render_with_cookie(tpl, set_cookie)
        }
        Err(e) => html_error(e.status(), &e.public_message()),
    }
}

fn render_with_cookie<T: Template>(tpl: T, set_cookie: Option<String>) -> Response {
    match tpl.render() {
        Ok(html) => {
            let mut resp = (StatusCode::OK, Html(html)).into_response();
            if let Some(cookie) = set_cookie {
                if let Ok(v) = cookie.parse() {
                    resp.headers_mut().append(header::SET_COOKIE, v);
                }
            }
            resp
        }
        Err(e) => html_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("template error: {e}"),
        ),
    }
}

async fn render_list_with_error(
    state: &AppState,
    headers: &HeaderMap,
    csrf_token: String,
    set_cookie: Option<String>,
    query: String,
    message: String,
    status: StatusCode,
) -> Response {
    // Error pages fall back to the default view; the transient message is
    // what matters, not the previous filter state.
    let status_filter = StatusFilter::All;
    let sort = LinkSort::Newest;
    let (page, per_page) = normalize_pagination(None, None);
    let now = now_millis();
    let rows = list_links(
        &state.db,
        ListParams {
            query: if query.trim().is_empty() {
                None
            } else {
                Some(query.clone())
            },
            status: status_filter,
            sort,
            page,
            per_page,
            now,
        },
    )
    .await
    .map(|r| {
        r.items
            .into_iter()
            .map(|item| to_row(state, item, now))
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    let tpl = LinksTemplate {
        title: "Admin — Links".to_string(),
        brand_host: brand_host(state),
        csrf_token,
        created_slug: None,
        created_short_url: None,
        error: Some(message),
        query,
        status: status_filter.as_param().to_string(),
        sort: sort.as_param().to_string(),
        links: rows,
        page,
        per_page,
        total: 0,
        total_pages: 1,
        range_start: 0,
        range_end: 0,
        prev_url: None,
        next_url: None,
        list_url: list_url("", status_filter, sort, per_page, Some(page)),
    };
    match tpl.render() {
        Ok(html) => {
            let mut resp = (status, Html(html)).into_response();
            if let Some(cookie) = set_cookie {
                if let Ok(v) = cookie.parse() {
                    resp.headers_mut().append(header::SET_COOKIE, v);
                }
            }
            // Refresh cookie even on error so the form stays usable.
            if resp.headers().get(header::SET_COOKIE).is_none() {
                let _ = &headers;
            }
            resp
        }
        Err(e) => html_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

pub async fn admin_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<CreateForm>,
) -> Response {
    if let Err(e) = check_browser_mutation(&headers, &state, form.csrf_token.as_deref()) {
        return html_error(e.status(), &e.public_message());
    }
    let (csrf_token, set_cookie) = ensure_csrf_token(&headers, &state);

    let expires_at = match parse_admin_expires(form.expires_at.as_deref()) {
        Ok(v) => v,
        Err(e) => {
            return render_list_with_error(
                &state,
                &headers,
                csrf_token,
                set_cookie,
                String::new(),
                e.public_message(),
                e.status(),
            )
            .await
        }
    };

    let input = CreateLinkInput {
        target_url: form.target_url.clone().unwrap_or_default(),
        custom_slug: form.custom_slug.clone().filter(|s| !s.trim().is_empty()),
        label: form.label.clone().filter(|s| !s.trim().is_empty()),
        expires_at,
    };

    match create_link(&state.db, &state.config.base_url, input).await {
        Ok(link) => {
            let mut resp = (
                StatusCode::SEE_OTHER,
                [(header::LOCATION, format!("/admin?created={}", link.slug))],
                "",
            )
                .into_response();
            if let Some(cookie) = set_cookie {
                if let Ok(v) = cookie.parse() {
                    resp.headers_mut().append(header::SET_COOKIE, v);
                }
            }
            resp
        }
        Err(e) => {
            render_list_with_error(
                &state,
                &headers,
                csrf_token,
                set_cookie,
                String::new(),
                e.public_message(),
                e.status(),
            )
            .await
        }
    }
}

pub async fn admin_edit_form(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    let (csrf_token, set_cookie) = ensure_csrf_token(&headers, &state);
    match get_link(&state.db, &slug).await {
        Ok(link) => {
            let tpl = EditTemplate {
                title: format!("Edit {}", link.slug),
                brand_host: brand_host(&state),
                csrf_token,
                short_url: state.short_url(&link.slug),
                slug: link.slug.clone(),
                target_url: link.target_url.clone(),
                label: link.label.clone().unwrap_or_default(),
                expires_input: format_datetime_local(link.expires_at),
                expires_display: link
                    .expires_at
                    .map(millis_to_rfc3339)
                    .unwrap_or_else(|| "never".to_string()),
                created_at: millis_to_rfc3339(link.created_at),
                updated_at: millis_to_rfc3339(link.updated_at),
                disabled: link.is_disabled(),
                error: None,
            };
            render_with_cookie(tpl, set_cookie)
        }
        Err(e) => html_error(e.status(), &e.public_message()),
    }
}

pub async fn admin_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    axum::Form(form): axum::Form<UpdateForm>,
) -> Response {
    if let Err(e) = check_browser_mutation(&headers, &state, form.csrf_token.as_deref()) {
        return html_error(e.status(), &e.public_message());
    }
    let (csrf_token, set_cookie) = ensure_csrf_token(&headers, &state);

    let expires_at = match parse_admin_expires(form.expires_at.as_deref()) {
        Ok(None) => Some(None),
        Ok(Some(v)) => Some(Some(v)),
        Err(e) => {
            return render_edit_with_error(
                &state,
                &slug,
                csrf_token,
                set_cookie,
                e.public_message(),
                e.status(),
            )
            .await;
        }
    };

    // Empty target means "keep existing"? No — require a value; HTML form
    // always sends one. Treat empty string as validation error.
    let target_opt = form.target_url.clone().filter(|s| !s.is_empty());

    let input = UpdateLinkInput {
        target_url: target_opt,
        label: Some(form.label.clone().filter(|s| !s.trim().is_empty())),
        expires_at,
    };

    // If target was empty string, surface a clear error.
    if form.target_url.as_deref().unwrap_or("").is_empty() {
        return render_edit_with_error(
            &state,
            &slug,
            csrf_token,
            set_cookie,
            "target URL must not be empty".to_string(),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
    }

    match update_link(&state.db, &state.config.base_url, &slug, input).await {
        Ok(link) => {
            let mut resp = (
                StatusCode::SEE_OTHER,
                [(header::LOCATION, format!("/admin/links/{}", link.slug))],
                "",
            )
                .into_response();
            if let Some(cookie) = set_cookie {
                if let Ok(v) = cookie.parse() {
                    resp.headers_mut().append(header::SET_COOKIE, v);
                }
            }
            resp
        }
        Err(e) => {
            render_edit_with_error(
                &state,
                &slug,
                csrf_token,
                set_cookie,
                e.public_message(),
                e.status(),
            )
            .await
        }
    }
}

async fn render_edit_with_error(
    state: &AppState,
    slug: &str,
    csrf_token: String,
    set_cookie: Option<String>,
    message: String,
    status: StatusCode,
) -> Response {
    match get_link(&state.db, slug).await {
        Ok(link) => {
            let tpl = EditTemplate {
                title: format!("Edit {}", link.slug),
                brand_host: brand_host(state),
                csrf_token,
                short_url: state.short_url(&link.slug),
                slug: link.slug.clone(),
                target_url: link.target_url.clone(),
                label: link.label.clone().unwrap_or_default(),
                expires_input: format_datetime_local(link.expires_at),
                expires_display: link
                    .expires_at
                    .map(millis_to_rfc3339)
                    .unwrap_or_else(|| "never".to_string()),
                created_at: millis_to_rfc3339(link.created_at),
                updated_at: millis_to_rfc3339(link.updated_at),
                disabled: link.is_disabled(),
                error: Some(message),
            };
            match tpl.render() {
                Ok(html) => {
                    let mut resp = (status, Html(html)).into_response();
                    if let Some(cookie) = set_cookie {
                        if let Ok(v) = cookie.parse() {
                            resp.headers_mut().append(header::SET_COOKIE, v);
                        }
                    }
                    resp
                }
                Err(e) => html_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
            }
        }
        Err(e) => html_error(e.status(), &e.public_message()),
    }
}

pub async fn admin_disable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    axum::Form(form): axum::Form<EmptyForm>,
) -> Response {
    if let Err(e) = check_browser_mutation(&headers, &state, form.csrf_token.as_deref()) {
        return html_error(e.status(), &e.public_message());
    }
    let (_, set_cookie) = ensure_csrf_token(&headers, &state);
    match set_disabled(&state.db, &slug, true).await {
        Ok(link) => {
            let mut resp = (
                StatusCode::SEE_OTHER,
                [(
                    header::LOCATION,
                    safe_return_to(form.return_to.as_deref(), &link.slug),
                )],
                "",
            )
                .into_response();
            if let Some(cookie) = set_cookie {
                if let Ok(v) = cookie.parse() {
                    resp.headers_mut().append(header::SET_COOKIE, v);
                }
            }
            resp
        }
        Err(e) => html_error(e.status(), &e.public_message()),
    }
}

pub async fn admin_enable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    axum::Form(form): axum::Form<EmptyForm>,
) -> Response {
    if let Err(e) = check_browser_mutation(&headers, &state, form.csrf_token.as_deref()) {
        return html_error(e.status(), &e.public_message());
    }
    let (_, set_cookie) = ensure_csrf_token(&headers, &state);
    match set_disabled(&state.db, &slug, false).await {
        Ok(link) => {
            let mut resp = (
                StatusCode::SEE_OTHER,
                [(
                    header::LOCATION,
                    safe_return_to(form.return_to.as_deref(), &link.slug),
                )],
                "",
            )
                .into_response();
            if let Some(cookie) = set_cookie {
                if let Ok(v) = cookie.parse() {
                    resp.headers_mut().append(header::SET_COOKIE, v);
                }
            }
            resp
        }
        Err(e) => html_error(e.status(), &e.public_message()),
    }
}

// Keep compiler happy about unused import alias without warnings elsewhere.
#[allow(dead_code)]
fn _field_name() -> &'static str {
    CSRF_FORM_FIELD
}
