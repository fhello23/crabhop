//! SQLite repository for links. Thin SQL only; validation lives in `domain`.

use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use crate::domain::link::{
    generate_slug, normalize_custom_slug, validate_label, validate_target_url, CreateLinkInput,
    UpdateLinkInput, MAX_CREATE_RETRIES,
};
use crate::error::{is_unique_violation, AppError};
use crate::state::now_millis;
use url::Url;

#[derive(Debug, Clone, FromRow, serde::Serialize)]
pub struct Link {
    pub id: String,
    pub slug: String,
    pub target_url: String,
    pub label: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: Option<i64>,
    pub disabled_at: Option<i64>,
}

impl Link {
    pub fn is_disabled(&self) -> bool {
        self.disabled_at.is_some()
    }

    pub fn is_expired(&self, now: i64) -> bool {
        matches!(self.expires_at, Some(e) if e <= now)
    }
}

pub async fn get_link(pool: &SqlitePool, slug: &str) -> Result<Link, AppError> {
    let slug = slug.to_ascii_lowercase();
    let link = sqlx::query_as::<_, Link>("SELECT * FROM links WHERE slug = ?")
        .bind(slug)
        .fetch_optional(pool)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(link)
}

pub async fn create_link(
    pool: &SqlitePool,
    base_url: &Url,
    input: CreateLinkInput,
) -> Result<Link, AppError> {
    let target_url = validate_target_url(&input.target_url, base_url)?;
    let label = validate_label(input.label.as_deref())?;

    if let Some(exp) = input.expires_at {
        if exp <= now_millis() {
            return Err(AppError::Validation(
                "expires_at must be in the future".to_string(),
            ));
        }
    }

    // Custom slug path: single attempt, conflict surfaces as 409.
    if let Some(raw) = input.custom_slug {
        if raw.trim().is_empty() {
            // Treat empty custom slug as "generate one".
            return create_with_generated_slug(pool, target_url, label, input.expires_at).await;
        }
        let slug = normalize_custom_slug(&raw)?;
        return insert_link(pool, slug, target_url, label, input.expires_at).await;
    }

    create_with_generated_slug(pool, target_url, label, input.expires_at).await
}

async fn create_with_generated_slug(
    pool: &SqlitePool,
    target_url: String,
    label: Option<String>,
    expires_at: Option<i64>,
) -> Result<Link, AppError> {
    for _ in 0..MAX_CREATE_RETRIES {
        let slug = generate_slug();
        match insert_link(pool, slug, target_url.clone(), label.clone(), expires_at).await {
            Err(AppError::Conflict) => continue,
            other => return other,
        }
    }
    Err(AppError::internal(anyhow::anyhow!(
        "slug collision retry budget exhausted"
    )))
}

async fn insert_link(
    pool: &SqlitePool,
    slug: String,
    target_url: String,
    label: Option<String>,
    expires_at: Option<i64>,
) -> Result<Link, AppError> {
    let now = now_millis();
    let id = Uuid::new_v4().to_string();
    let res = sqlx::query(
        "INSERT INTO links (id, slug, target_url, label, created_at, updated_at, expires_at, disabled_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL)",
    )
    .bind(&id)
    .bind(&slug)
    .bind(&target_url)
    .bind(&label)
    .bind(now)
    .bind(now)
    .bind(expires_at)
    .execute(pool)
    .await;

    match res {
        Ok(_) => get_link(pool, &slug).await,
        Err(e) if is_unique_violation(&e) => Err(AppError::Conflict),
        Err(e) => Err(AppError::from(e)),
    }
}

#[derive(Debug)]
pub struct ListParams {
    pub query: Option<String>,
    pub page: u32,
    pub per_page: u32,
}

#[derive(Debug)]
pub struct ListResult {
    pub items: Vec<Link>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}

pub fn normalize_pagination(page: Option<i64>, per_page: Option<i64>) -> (u32, u32) {
    let page = page.unwrap_or(1).clamp(1, 10_000) as u32;
    let per_page = per_page.unwrap_or(20).clamp(1, 100) as u32;
    (page, per_page)
}

pub async fn list_links(pool: &SqlitePool, params: ListParams) -> Result<ListResult, AppError> {
    let page = params.page.max(1);
    let per_page = params.per_page.clamp(1, 100);
    let offset = ((page - 1) as i64) * (per_page as i64);

    let (count_sql, list_sql) = match params.query {
        Some(ref q) if !q.trim().is_empty() => (
            "SELECT COUNT(*) FROM links WHERE slug LIKE ? OR target_url LIKE ? OR label LIKE ?",
            "SELECT * FROM links WHERE slug LIKE ? OR target_url LIKE ? OR label LIKE ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
        ),
        _ => (
            "SELECT COUNT(*) FROM links",
            "SELECT * FROM links ORDER BY created_at DESC LIMIT ? OFFSET ?",
        ),
    };

    let like_opt = params.query.as_ref().and_then(|q| {
        let t = q.trim();
        if t.is_empty() {
            None
        } else {
            Some(format!("%{t}%"))
        }
    });

    let total: i64 = match like_opt {
        Some(ref like) => {
            sqlx::query_scalar(count_sql)
                .bind(like)
                .bind(like)
                .bind(like)
                .fetch_one(pool)
                .await?
        }
        None => sqlx::query_scalar(count_sql).fetch_one(pool).await?,
    };

    let items: Vec<Link> = match like_opt {
        Some(ref like) => {
            sqlx::query_as::<_, Link>(list_sql)
                .bind(like)
                .bind(like)
                .bind(like)
                .bind(per_page as i64)
                .bind(offset)
                .fetch_all(pool)
                .await?
        }
        None => {
            sqlx::query_as::<_, Link>(list_sql)
                .bind(per_page as i64)
                .bind(offset)
                .fetch_all(pool)
                .await?
        }
    };

    Ok(ListResult {
        items,
        total,
        page,
        per_page,
    })
}

pub async fn update_link(
    pool: &SqlitePool,
    base_url: &Url,
    slug: &str,
    input: UpdateLinkInput,
) -> Result<Link, AppError> {
    let existing = get_link(pool, slug).await?;
    let mut target_url = existing.target_url.clone();
    let mut label = existing.label.clone();
    let mut expires_at = existing.expires_at;

    if let Some(raw) = input.target_url {
        target_url = validate_target_url(&raw, base_url)?;
    }
    if let Some(label_in) = input.label {
        label = validate_label(label_in.as_deref())?;
    }
    if let Some(exp_in) = input.expires_at {
        expires_at = exp_in;
        if let Some(e) = expires_at {
            if e <= now_millis() {
                return Err(AppError::Validation(
                    "expires_at must be in the future".to_string(),
                ));
            }
        }
    }

    let now = now_millis();
    sqlx::query(
        "UPDATE links SET target_url = ?, label = ?, expires_at = ?, updated_at = ? WHERE slug = ?",
    )
    .bind(&target_url)
    .bind(&label)
    .bind(expires_at)
    .bind(now)
    .bind(existing.slug.clone())
    .execute(pool)
    .await?;

    get_link(pool, &existing.slug).await
}

pub async fn set_disabled(pool: &SqlitePool, slug: &str, disabled: bool) -> Result<Link, AppError> {
    let existing = get_link(pool, slug).await?;
    let now = now_millis();
    if disabled {
        sqlx::query("UPDATE links SET disabled_at = ?, updated_at = ? WHERE slug = ?")
            .bind(now)
            .bind(now)
            .bind(&existing.slug)
            .execute(pool)
            .await?;
    } else {
        sqlx::query("UPDATE links SET disabled_at = NULL, updated_at = ? WHERE slug = ?")
            .bind(now)
            .bind(&existing.slug)
            .execute(pool)
            .await?;
    }
    get_link(pool, &existing.slug).await
}
