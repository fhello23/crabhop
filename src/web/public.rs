use askama::Template;
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{Html, IntoResponse, Response};

use crate::db::links::{get_link, Link};

#[derive(Template)]
#[template(path = "shared_text.html")]
struct SharedTextTemplate {
    slug: String,
    title: String,
    text_content: String,
}
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

/// GET /{slug} serves a redirect or read-only text page. Axum strips HEAD
/// bodies; only successful GET visits count toward analytics.
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
                // Never wait on analytics I/O or consume a request connection.
                if !state
                    .analytics
                    .try_record(resolved.id.clone(), now_millis())
                {
                    tracing::warn!("redirect analytics queue unavailable; dropping click");
                }
            }
            let mut resp = if let Some(text_content) = resolved.text_content {
                let template = SharedTextTemplate {
                    slug: resolved.slug,
                    title: resolved.label.unwrap_or_else(|| "Shared text".to_string()),
                    text_content,
                };
                match template.render() {
                    Ok(html) => (StatusCode::OK, Html(html)).into_response(),
                    Err(e) => return AppError::internal(e).into_response(),
                }
            } else {
                (
                    StatusCode::FOUND,
                    [
                        (header::LOCATION, resolved.target_url.as_str()),
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                )
                    .into_response()
            };
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
            resp.headers_mut().insert(header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'".parse().unwrap());
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

/// Download the original UTF-8 bytes, including original line endings. Downloads
/// are not additional page views and use the same availability checks as shares.
pub async fn download_text(
    State(state): State<AppState>,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> Response {
    let result = async {
        let link = resolve_redirect(&state, &slug).await?;
        let text = link.text_content.ok_or(AppError::NotFound)?;
        let disposition = axum::http::HeaderValue::from_str(&format!(
            "attachment; filename=\"{}.txt\"",
            link.slug
        ))
        .map_err(AppError::internal)?;
        Ok::<_, AppError>(
            (
                [
                    (
                        header::CONTENT_TYPE,
                        axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
                    ),
                    (header::CONTENT_DISPOSITION, disposition),
                ],
                text,
            )
                .into_response(),
        )
    }
    .await;
    let mut response = result.unwrap_or_else(IntoResponse::into_response);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-robots-tag", "noindex, nofollow".parse().unwrap());
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; sandbox".parse().unwrap(),
    );
    response
}

async fn resolve_redirect(state: &AppState, slug: &str) -> Result<Link, AppError> {
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
    Ok(link)
}
