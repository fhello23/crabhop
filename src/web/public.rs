use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};

use crate::db::analytics::record_click;
use crate::db::links::get_link;
use crate::error::AppError;
use crate::state::{now_millis, AppState};

pub async fn landing(State(state): State<AppState>) -> Html<String> {
    let host = state.config.base_url.host_str().unwrap_or("shortener");
    Html(format!(
        "<!doctype html><html><head><title>{host}</title></head>\
         <body><h1>{host}</h1><p>Private link shortener.</p>\
         <p><small>Want your own? Check it out at \
         <a href=\"https://github.com/fhello23/crabhop\">github.com/fhello23/crabhop</a>.</small></p></body></html>"
    ))
}

pub async fn robots_txt() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "User-agent: *\nDisallow: /\n",
    )
}

pub async fn health_live() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"status":"live"}"#,
    )
}

pub async fn health_ready(State(state): State<AppState>) -> impl IntoResponse {
    // Lightweight DB check; migrations already ran at startup so a reachable
    // DB implies current schema.
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.db)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"status":"ready"}"#.to_string(),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"status":"not-ready"}"#.to_string(),
        ),
    }
}

/// GET /{slug} — public redirect. HEAD is routed to the same handler;
/// the empty body makes GET/HEAD behaviorally identical for headers.
/// Only successful GET redirects are counted; HEAD exists for health
/// checks and must not inflate analytics.
pub async fn redirect_slug(
    State(state): State<AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    // Tracing: log only method/path outcome, never headers or URLs.
    let _ = &headers;
    match resolve_redirect(&state, &slug).await {
        Ok(resolved) => {
            if method == Method::GET {
                // Best-effort analytics: a recording failure must never
                // break the redirect itself.
                if record_click(&state.db, &resolved.link_id, now_millis())
                    .await
                    .is_err()
                {
                    tracing::warn!("failed to record redirect analytics");
                }
            }
            let mut resp = (
                StatusCode::FOUND,
                [
                    (header::LOCATION, resolved.target_url.as_str()),
                    (header::CACHE_CONTROL, "no-store"),
                ],
            )
                .into_response();
            resp.headers_mut().insert(
                axum::http::HeaderName::from_static("x-robots-tag"),
                axum::http::HeaderValue::from_static("noindex, nofollow"),
            );
            resp
        }
        Err(AppError::NotFound) => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            "not found",
        )
            .into_response(),
        Err(AppError::Gone) => (
            StatusCode::GONE,
            [(header::CACHE_CONTROL, "no-store")],
            "link has expired",
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

struct ResolvedRedirect {
    link_id: String,
    target_url: String,
}

async fn resolve_redirect(state: &AppState, slug: &str) -> Result<ResolvedRedirect, AppError> {
    let link = get_link(&state.db, slug).await?;
    if link.is_disabled() {
        return Err(AppError::NotFound);
    }
    if link.is_expired(now_millis()) {
        return Err(AppError::Gone);
    }
    // Defensive: stored URLs were validated at write time, but re-check for
    // control characters before reflecting into the Location header.
    if link.target_url.chars().any(|c| c.is_control()) {
        return Err(AppError::internal(anyhow::anyhow!(
            "stored target failed header safety check"
        )));
    }
    Ok(ResolvedRedirect {
        link_id: link.id,
        target_url: link.target_url,
    })
}
