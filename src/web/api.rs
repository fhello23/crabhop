use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::db::links::{
    create_link, get_link, list_links, normalize_pagination, set_disabled, update_link, Link,
    LinkSort, ListParams, StatusFilter,
};
use crate::domain::link::{CreateLinkInput, UpdateLinkInput};
use crate::error::AppError;
use crate::state::{millis_to_rfc3339, now_millis, parse_expires_at, AppState};
use crate::web::security::{check_api_mutation_headers, require_json_content_type};

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct LinkResponse {
    pub slug: String,
    pub short_url: String,
    pub target_url: String,
    pub label: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: Option<String>,
    pub disabled: bool,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub data: Vec<LinkResponse>,
    pub page: u32,
    pub per_page: u32,
    pub total: i64,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    pub message: String,
    pub code: u16,
}

#[derive(Debug, Deserialize)]
pub struct ApiListQuery {
    pub q: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiCreateBody {
    pub target_url: String,
    #[serde(default)]
    pub custom_slug: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub expires_at: Option<ExpiresAt>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiPatchBody {
    #[serde(default)]
    pub target_url: Option<String>,
    #[serde(default, deserialize_with = "opt_opt_string")]
    pub label: Option<Option<String>>,
    #[serde(default, deserialize_with = "opt_opt_expires")]
    pub expires_at: Option<Option<ExpiresAt>>,
    #[serde(default, deserialize_with = "opt_opt_string")]
    pub _unused_guard: Option<Option<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ExpiresAt {
    Rfc3339(String),
    Millis(i64),
}

impl ExpiresAt {
    fn to_millis(&self) -> Result<i64, AppError> {
        match self {
            Self::Millis(m) => {
                if *m <= 0 {
                    return Err(AppError::Validation(
                        "expires_at must be a positive timestamp".to_string(),
                    ));
                }
                Ok(*m)
            }
            Self::Rfc3339(s) => parse_expires_at(s).ok_or_else(|| {
                AppError::Validation("expires_at must be RFC 3339 or Unix milliseconds".to_string())
            }),
        }
    }
}

fn opt_opt_string<'de, D>(d: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(d)?;
    Ok(Some(opt))
}

fn opt_opt_expires<'de, D>(d: D) -> Result<Option<Option<ExpiresAt>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<ExpiresAt> = Option::deserialize(d)?;
    Ok(Some(opt))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn to_response(state: &AppState, link: &Link) -> LinkResponse {
    LinkResponse {
        short_url: state.short_url(&link.slug),
        slug: link.slug.clone(),
        target_url: link.target_url.clone(),
        label: link.label.clone(),
        created_at: millis_to_rfc3339(link.created_at),
        updated_at: millis_to_rfc3339(link.updated_at),
        expires_at: link.expires_at.map(millis_to_rfc3339),
        disabled: link.is_disabled(),
    }
}

fn json_error(e: AppError) -> Response {
    let status = e.status();
    let body = ErrorBody {
        error: ErrorDetail {
            message: e.public_message(),
            code: status.as_u16(),
        },
    };
    (status, axum::Json(body)).into_response()
}

async fn read_json_body<T: serde::de::DeserializeOwned>(
    headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Result<T, AppError> {
    require_json_content_type(headers)?;
    if body.len() > 16 * 1024 {
        return Err(AppError::PayloadTooLarge);
    }
    serde_json::from_slice::<T>(&body)
        .map_err(|e| AppError::Validation(format!("invalid request body: {e}")))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn api_list(State(state): State<AppState>, Query(q): Query<ApiListQuery>) -> Response {
    let (page, per_page) = normalize_pagination(q.page, q.per_page);
    match list_links(
        &state.db,
        ListParams {
            query: q.q.filter(|s| !s.trim().is_empty()),
            status: StatusFilter::All,
            sort: LinkSort::Newest,
            page,
            per_page,
            now: now_millis(),
        },
    )
    .await
    {
        Ok(result) => {
            let data = result
                .items
                .iter()
                .map(|item| to_response(&state, &item.link))
                .collect();
            let body = ListResponse {
                data,
                page: result.page,
                per_page: result.per_page,
                total: result.total,
            };
            (StatusCode::OK, axum::Json(body)).into_response()
        }
        Err(e) => json_error(e),
    }
}

pub async fn api_get(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    match get_link(&state.db, &slug).await {
        Ok(link) => (StatusCode::OK, axum::Json(to_response(&state, &link))).into_response(),
        Err(e) => json_error(e),
    }
}

pub async fn api_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(e) = check_api_mutation_headers(&headers) {
        return json_error(e);
    }
    let parsed: ApiCreateBody = match read_json_body(&headers, body).await {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let expires_at = match parsed.expires_at.map(|e| e.to_millis()).transpose() {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let input = CreateLinkInput {
        target_url: parsed.target_url,
        custom_slug: parsed.custom_slug.filter(|s| !s.trim().is_empty()),
        label: parsed.label.filter(|s| !s.trim().is_empty()),
        expires_at,
    };
    match create_link(&state.db, &state.config.base_url, input).await {
        Ok(link) => (
            StatusCode::CREATED,
            [(header::LOCATION, state.short_url(&link.slug))],
            axum::Json(to_response(&state, &link)),
        )
            .into_response(),
        Err(e) => json_error(e),
    }
}

pub async fn api_patch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(e) = check_api_mutation_headers(&headers) {
        return json_error(e);
    }
    // Guard against struct-level unknown-field bypass: ApiPatchBody denies
    // unknown fields, and an empty JSON object is a no-op validation error.
    let parsed: ApiPatchBody = match read_json_body(&headers, body).await {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    // Remove the guard field (always None unless caller sent `_unused_guard`,
    // which deny_unknown_fields would already have rejected).
    if parsed._unused_guard.is_some() {
        return json_error(AppError::Validation("unsupported field".to_string()));
    }
    let expires_at: Option<Option<i64>> = match parsed
        .expires_at
        .map(|opt| opt.map(|e| e.to_millis()).transpose())
        .transpose()
    {
        Ok(v) => v,
        Err(e) => return json_error(e),
    };
    let input = UpdateLinkInput {
        target_url: parsed.target_url,
        label: parsed.label,
        expires_at,
    };
    // Reject empty patch (no updatable fields present).
    if input.target_url.is_none() && input.label.is_none() && input.expires_at.is_none() {
        return json_error(AppError::Validation(
            "patch body must include at least one of: target_url, label, expires_at".to_string(),
        ));
    }
    match update_link(&state.db, &state.config.base_url, &slug, input).await {
        Ok(link) => (StatusCode::OK, axum::Json(to_response(&state, &link))).into_response(),
        Err(e) => json_error(e),
    }
}

pub async fn api_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    if let Err(e) = check_api_mutation_headers(&headers) {
        return json_error(e);
    }
    // DELETE has no JSON body; enforce custom header only (Content-Type N/A).
    // Soft-disable: row is kept, redirects become 404.
    match set_disabled(&state.db, &slug, true).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => json_error(e),
    }
}

pub async fn api_enable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> Response {
    if let Err(e) = check_api_mutation_headers(&headers) {
        return json_error(e);
    }
    match set_disabled(&state.db, &slug, false).await {
        Ok(link) => (StatusCode::OK, axum::Json(to_response(&state, &link))).into_response(),
        Err(e) => json_error(e),
    }
}
