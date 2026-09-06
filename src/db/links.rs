//! SQLite repository for links. Thin SQL only; validation lives in `domain`.

use sqlx::{FromRow, SqlitePool};
use uuid::Uuid;

use crate::domain::link::{
    generate_slug, normalize_custom_slug, validate_expiration, validate_label, validate_target_url,
    validate_text_content, CreateLinkInput, UpdateLinkInput, MAX_CREATE_RETRIES,
};
use crate::error::{is_unique_violation, AppError};
use crate::state::now_millis;
use url::Url;

#[derive(Debug, Clone, FromRow, serde::Serialize)]
pub struct Link {
    pub id: String,
    pub slug: String,
    pub target_url: String,
    pub text_content: Option<String>,
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
    let target_url = if let Some(text) = &input.text_content {
        if !input.target_url.is_empty() {
            return Err(AppError::Validation(
                "provide either target_url or text_content, not both".to_string(),
            ));
        }
        validate_text_content(text)?;
        String::new()
    } else {
        validate_target_url(&input.target_url, base_url)?
    };
    let label = validate_label(input.label.as_deref())?;

    validate_expiration(input.expires_at, now_millis())?;

    // Custom slug path: single attempt, conflict surfaces as 409.
    if let Some(raw) = input.custom_slug {
        if raw.trim().is_empty() {
            // Treat empty custom slug as "generate one".
            return create_with_generated_slug(
                pool,
                target_url,
                input.text_content,
                label,
                input.expires_at,
            )
            .await;
        }
        let slug = normalize_custom_slug(&raw)?;
        return insert_link(
            pool,
            slug,
            target_url,
            input.text_content,
            label,
            input.expires_at,
        )
        .await;
    }

    create_with_generated_slug(
        pool,
        target_url,
        input.text_content,
        label,
        input.expires_at,
    )
    .await
}

async fn create_with_generated_slug(
    pool: &SqlitePool,
    target_url: String,
    text_content: Option<String>,
    label: Option<String>,
    expires_at: Option<i64>,
) -> Result<Link, AppError> {
    for _ in 0..MAX_CREATE_RETRIES {
        let slug = generate_slug();
        match insert_link(
            pool,
            slug,
            target_url.clone(),
            text_content.clone(),
            label.clone(),
            expires_at,
        )
        .await
        {
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
    text_content: Option<String>,
    label: Option<String>,
    expires_at: Option<i64>,
) -> Result<Link, AppError> {
    let now = now_millis();
    let id = Uuid::new_v4().to_string();
    let res = sqlx::query_as::<_, Link>(
        "INSERT INTO links (id, slug, target_url, text_content, label, created_at, updated_at, expires_at, disabled_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL) RETURNING *",
    )
    .bind(&id)
    .bind(&slug)
    .bind(&target_url)
    .bind(&text_content)
    .bind(&label)
    .bind(now)
    .bind(now)
    .bind(expires_at)
    .fetch_one(pool)
    .await;

    match res {
        Ok(link) => Ok(link),
        Err(e) if is_unique_violation(&e) => Err(AppError::Conflict),
        Err(e) => Err(AppError::from(e)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusFilter {
    #[default]
    All,
    Active,
    Expired,
    Disabled,
}

impl StatusFilter {
    /// Parse a query value. Missing/empty means All; anything else must be a
    /// known filter so API callers get feedback (the HTML UI falls back).
    pub fn from_param(raw: Option<&str>) -> Result<Self, AppError> {
        match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "" | "all" => Ok(Self::All),
            "active" => Ok(Self::Active),
            "expired" => Ok(Self::Expired),
            "disabled" => Ok(Self::Disabled),
            other => Err(AppError::Validation(format!(
                "invalid status filter {other:?}: expected all|active|expired|disabled"
            ))),
        }
    }

    pub fn as_param(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Active => "active",
            Self::Expired => "expired",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KindFilter {
    #[default]
    All,
    Url,
    Text,
}

impl KindFilter {
    pub fn from_param(raw: Option<&str>) -> Result<Self, AppError> {
        match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "" | "all" => Ok(Self::All),
            "url" => Ok(Self::Url),
            "text" => Ok(Self::Text),
            other => Err(AppError::Validation(format!(
                "invalid kind filter {other:?}: expected all|url|text"
            ))),
        }
    }

    pub fn as_param(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Url => "url",
            Self::Text => "text",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkSort {
    #[default]
    Newest,
    Oldest,
    RecentlyUpdated,
    SlugAsc,
    MostClicked,
}

impl LinkSort {
    pub fn from_param(raw: Option<&str>) -> Result<Self, AppError> {
        match raw.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "" | "newest" => Ok(Self::Newest),
            "oldest" => Ok(Self::Oldest),
            "updated" => Ok(Self::RecentlyUpdated),
            "slug" => Ok(Self::SlugAsc),
            "clicked" => Ok(Self::MostClicked),
            other => Err(AppError::Validation(format!(
                "invalid sort {other:?}: expected newest|oldest|updated|slug|clicked"
            ))),
        }
    }

    pub fn as_param(self) -> &'static str {
        match self {
            Self::Newest => "newest",
            Self::Oldest => "oldest",
            Self::RecentlyUpdated => "updated",
            Self::SlugAsc => "slug",
            Self::MostClicked => "clicked",
        }
    }
}

#[derive(Debug)]
pub struct ListParams {
    pub query: Option<String>,
    pub status: StatusFilter,
    pub kind: KindFilter,
    pub sort: LinkSort,
    pub page: u32,
    pub per_page: u32,
    /// Reference time for active/expired evaluation (Unix millis, UTC).
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct LinkListItem {
    pub link: Link,
    pub total_clicks: i64,
    pub last_clicked_at: Option<i64>,
}

#[derive(Debug)]
pub struct ListResult {
    pub items: Vec<LinkListItem>,
    pub total: i64,
    pub page: u32,
    pub per_page: u32,
}

/// Flat row for the list query: every link column plus click aggregates.
#[derive(Debug, FromRow)]
struct LinkListRow {
    id: String,
    slug: String,
    target_url: String,
    text_content: Option<String>,
    label: Option<String>,
    created_at: i64,
    updated_at: i64,
    expires_at: Option<i64>,
    disabled_at: Option<i64>,
    total_clicks: i64,
    last_clicked_at: Option<i64>,
}

impl From<LinkListRow> for LinkListItem {
    fn from(r: LinkListRow) -> Self {
        Self {
            link: Link {
                id: r.id,
                slug: r.slug,
                target_url: r.target_url,
                text_content: r.text_content,
                label: r.label,
                created_at: r.created_at,
                updated_at: r.updated_at,
                expires_at: r.expires_at,
                disabled_at: r.disabled_at,
            },
            total_clicks: r.total_clicks,
            last_clicked_at: r.last_clicked_at,
        }
    }
}

pub fn normalize_pagination(page: Option<i64>, per_page: Option<i64>) -> (u32, u32) {
    let page = page.unwrap_or(1).clamp(1, 10_000) as u32;
    let per_page = per_page.unwrap_or(20).clamp(1, 100) as u32;
    (page, per_page)
}

/// Status predicate fragment. Disabled takes precedence over expired, so the
/// expired predicate only matches enabled links.
fn status_predicate(status: StatusFilter) -> &'static str {
    match status {
        StatusFilter::All => "",
        StatusFilter::Active => "disabled_at IS NULL AND (expires_at IS NULL OR expires_at > ?)",
        StatusFilter::Expired => {
            "disabled_at IS NULL AND expires_at IS NOT NULL AND expires_at <= ?"
        }
        StatusFilter::Disabled => "disabled_at IS NOT NULL",
    }
}

/// Ordering fragment. Every ordering ends in a stable secondary key; column
/// names come only from this fixed match, never from query input.
fn order_clause(sort: LinkSort) -> &'static str {
    match sort {
        LinkSort::Newest => "created_at DESC, slug ASC",
        LinkSort::Oldest => "created_at ASC, slug ASC",
        LinkSort::RecentlyUpdated => "updated_at DESC, slug ASC",
        LinkSort::SlugAsc => "slug ASC",
        LinkSort::MostClicked => {
            "(SELECT COALESCE(SUM(click_count), 0) FROM link_daily_clicks WHERE link_id = links.id) DESC, slug ASC"
        }
    }
}

pub async fn list_links(pool: &SqlitePool, params: ListParams) -> Result<ListResult, AppError> {
    let per_page = params.per_page.clamp(1, 100);
    let needs_now = !matches!(params.status, StatusFilter::All | StatusFilter::Disabled);

    let like_opt = params.query.as_ref().and_then(|q| {
        let t = q.trim();
        if t.is_empty() {
            None
        } else {
            Some(format!("%{t}%"))
        }
    });

    let mut count_sql = String::from("SELECT COUNT(*) FROM links");
    let mut list_sql = String::from(
        "SELECT links.*,
            COALESCE((SELECT SUM(click_count) FROM link_daily_clicks WHERE link_id = links.id), 0) AS total_clicks,
            (SELECT MAX(last_clicked_at) FROM link_daily_clicks WHERE link_id = links.id) AS last_clicked_at
         FROM links",
    );
    let mut filters: Vec<&str> = Vec::new();
    let status_sql = status_predicate(params.status);
    if !status_sql.is_empty() {
        filters.push(status_sql);
    }
    match params.kind {
        KindFilter::All => {}
        KindFilter::Url => filters.push("text_content IS NULL"),
        KindFilter::Text => filters.push("text_content IS NOT NULL"),
    }
    if like_opt.is_some() {
        filters.push("(slug LIKE ? OR target_url LIKE ? OR label LIKE ?)");
    }
    if !filters.is_empty() {
        let clause = format!(" WHERE {}", filters.join(" AND "));
        count_sql.push_str(&clause);
        list_sql.push_str(&clause);
    }
    list_sql.push_str(" ORDER BY ");
    list_sql.push_str(order_clause(params.sort));
    list_sql.push_str(" LIMIT ? OFFSET ?");

    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
    if needs_now {
        count_q = count_q.bind(params.now);
    }
    if let Some(ref like) = like_opt {
        count_q = count_q.bind(like).bind(like).bind(like);
    }
    let total: i64 = count_q.fetch_one(pool).await?;

    // Clamp pages past the end so an emptied last page still shows content.
    let total_pages = ((total as f64) / (per_page as f64)).ceil() as u32;
    let page = params.page.max(1).min(total_pages.max(1));
    let offset = ((page - 1) as i64) * (per_page as i64);

    let mut list_q = sqlx::query_as::<_, LinkListRow>(&list_sql);
    if needs_now {
        list_q = list_q.bind(params.now);
    }
    if let Some(ref like) = like_opt {
        list_q = list_q.bind(like).bind(like).bind(like);
    }
    let rows: Vec<LinkListRow> = list_q
        .bind(per_page as i64)
        .bind(offset)
        .fetch_all(pool)
        .await?;

    Ok(ListResult {
        items: rows.into_iter().map(LinkListItem::from).collect(),
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
    // Link types are immutable: editing must not silently replace a redirect
    // with a text page (or vice versa).
    if (existing.text_content.is_some() && input.target_url.is_some())
        || (existing.text_content.is_none() && input.text_content.is_some())
    {
        return Err(AppError::Validation(
            "link type cannot be changed".to_string(),
        ));
    }
    if let Some(text) = &input.text_content {
        validate_text_content(text)?;
    }
    let target_url = input
        .target_url
        .as_deref()
        .map(|raw| validate_target_url(raw, base_url))
        .transpose()?;
    let label = input
        .label
        .as_ref()
        .map(|raw| validate_label(raw.as_deref()))
        .transpose()?;
    if let Some(expiry) = input.expires_at {
        validate_expiration(expiry, now_millis())?;
    }

    let now = now_millis();
    // Only write fields present in this patch. Copying omitted fields from
    // `existing` would overwrite another request's concurrent changes.
    // RETURNING also avoids a separate, fallible lookup after the commit.
    sqlx::query_as::<_, Link>(
        "UPDATE links SET
            target_url = COALESCE(?, target_url),
            text_content = COALESCE(?, text_content),
            label = CASE WHEN ? THEN ? ELSE label END,
            expires_at = CASE WHEN ? THEN ? ELSE expires_at END,
            updated_at = ?
         WHERE slug = ? RETURNING *",
    )
    .bind(&target_url)
    .bind(&input.text_content)
    .bind(label.is_some())
    .bind(label.flatten())
    .bind(input.expires_at.is_some())
    .bind(input.expires_at.flatten())
    .bind(now)
    .bind(existing.slug.clone())
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}

pub async fn set_disabled(pool: &SqlitePool, slug: &str, disabled: bool) -> Result<Link, AppError> {
    let now = now_millis();
    sqlx::query_as::<_, Link>(
        "UPDATE links SET disabled_at = ?, updated_at = ? WHERE slug = ? RETURNING *",
    )
    .bind(disabled.then_some(now))
    .bind(now)
    .bind(slug.to_ascii_lowercase())
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}
