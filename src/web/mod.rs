pub mod admin;
pub mod api;
pub mod public;
pub mod security;

use std::time::Duration;

use axum::http::{header, Method, StatusCode};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{delete, get, patch, post};
use axum::Router;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

pub fn app_router(state: AppState) -> Router {
    // Static routes first; catch-all slug route last so reserved paths
    // (/admin, /api, /health/*, /robots.txt, /static) always win.
    let router = Router::new()
        .route("/", get(public::landing))
        .route("/robots.txt", get(public::robots_txt))
        .route("/health/live", get(public::health_live))
        .route("/health/ready", get(public::health_ready))
        // Admin UI (browser forms; CSRF + Origin enforced in handlers).
        .route("/admin", get(admin::admin_list_with_created))
        .route("/admin/links", post(admin::admin_create))
        .route("/admin/links/{slug}", get(admin::admin_edit_form))
        .route("/admin/links/{slug}", post(admin::admin_update))
        .route("/admin/links/{slug}/disable", post(admin::admin_disable))
        .route("/admin/links/{slug}/enable", post(admin::admin_enable))
        // JSON API.
        .route("/api/v1/links", get(api::api_list))
        .route("/api/v1/links", post(api::api_create))
        .route("/api/v1/links/{slug}", get(api::api_get))
        .route("/api/v1/links/{slug}", patch(api::api_patch))
        .route("/api/v1/links/{slug}", delete(api::api_delete))
        .route("/api/v1/links/{slug}/enable", post(api::api_enable))
        // Static assets.
        .nest_service(
            "/static",
            tower_http::services::ServeDir::new("static").append_index_html_on_directories(false),
        )
        // Public redirect catch-all (GET + HEAD). Registered last.
        .route(
            "/{slug}",
            get(public::redirect_slug).head(public::redirect_slug),
        )
        .fallback(fallback_404)
        // 16 KiB body cap per plan (applies to form + JSON mutations).
        .layer(RequestBodyLimitLayer::new(16 * 1024))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(10),
        ))
        // Request logging deliberately excludes Authorization/Cookie headers
        // (TraceLayer default logs method/path/status/latency only).
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|req: &axum::http::Request<axum::body::Body>| {
                    tracing::info_span!(
                        "request",
                        method = %req.method(),
                        path = %req.uri().path(),
                    )
                })
                .on_response(
                    |res: &axum::http::Response<axum::body::Body>,
                     latency: Duration,
                     _span: &tracing::Span| {
                        tracing::info!(
                            status = %res.status().as_u16(),
                            latency_ms = %latency.as_millis(),
                            "response"
                        );
                    },
                ),
        )
        .layer(middleware::from_fn(security::security_headers_mw))
        .with_state(state);

    // Method-not-allowed nicety: Axum returns 405 automatically for known
    // paths with wrong method; fallback covers unknown paths.
    Router::new().merge(router)
}

async fn fallback_404(
    req: axum::http::Request<axum::body::Body>,
) -> impl axum::response::IntoResponse {
    let path = req.uri().path().to_owned();
    if path.starts_with("/api") {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":{"message":"not found","code":404}}"#,
        )
            .into_response();
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

#[allow(dead_code)]
fn _assert_methods() {
    // Route inventory matching plan §8 (GET /admin/links/{slug} edit form,
    // POST mutations only — no GET mutates state).
    let _: Method = Method::GET;
}
