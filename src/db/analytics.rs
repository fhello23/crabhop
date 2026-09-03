//! Daily click aggregates for public redirects.
//!
//! Privacy rules: one row per link per UTC day, counting only successful
//! public `GET` redirects. No IP addresses, referrers, or user agents are
//! stored. Counts are best-effort operational analytics: recording failures
//! never block a redirect.

use sqlx::SqlitePool;

use crate::error::AppError;

pub const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

/// Start of the UTC day containing `timestamp` (Unix millis).
pub fn day_start_utc(timestamp: i64) -> i64 {
    timestamp - timestamp.rem_euclid(MILLIS_PER_DAY)
}

#[derive(Debug, Clone)]
pub struct DailyClickPoint {
    pub day_start_utc: i64,
    pub click_count: i64,
}

#[derive(Debug, Clone)]
pub struct LinkActivitySummary {
    pub total_clicks: i64,
    pub last_7_days_clicks: i64,
    pub last_clicked_at: Option<i64>,
    /// `days` ordered buckets ending today (UTC), zero-filled.
    pub daily: Vec<DailyClickPoint>,
}

/// Record one click with a single atomic upsert.
pub async fn record_click(
    pool: &SqlitePool,
    link_id: &str,
    timestamp: i64,
) -> Result<(), AppError> {
    let day = day_start_utc(timestamp);
    sqlx::query(
        "INSERT INTO link_daily_clicks (link_id, day_start_utc, click_count, last_clicked_at)
         VALUES (?, ?, 1, ?)
         ON CONFLICT(link_id, day_start_utc) DO UPDATE SET
             click_count = click_count + 1,
             last_clicked_at = excluded.last_clicked_at",
    )
    .bind(link_id)
    .bind(day)
    .bind(timestamp)
    .execute(pool)
    .await?;
    Ok(())
}

/// Total clicks and last-clicked time for a single link.
pub async fn link_stats(pool: &SqlitePool, link_id: &str) -> Result<(i64, Option<i64>), AppError> {
    let total: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(click_count), 0) FROM link_daily_clicks WHERE link_id = ?",
    )
    .bind(link_id)
    .fetch_one(pool)
    .await?;
    let last: Option<i64> =
        sqlx::query_scalar("SELECT MAX(last_clicked_at) FROM link_daily_clicks WHERE link_id = ?")
            .bind(link_id)
            .fetch_one(pool)
            .await?;
    Ok((total, last))
}

/// Activity summary with a zero-filled daily series. `days` counts UTC
/// calendar days ending today; the "last 7 days" figure covers the seven
/// calendar days ending today regardless of `days`.
pub async fn get_link_activity(
    pool: &SqlitePool,
    link_id: &str,
    days: u32,
    timestamp: i64,
) -> Result<LinkActivitySummary, AppError> {
    let days = days.clamp(1, 366) as i64;
    let today = day_start_utc(timestamp);
    let series_start = today - (days - 1) * MILLIS_PER_DAY;
    let week_start = today - 6 * MILLIS_PER_DAY;

    let (total_clicks, last_clicked_at) = link_stats(pool, link_id).await?;

    let last_7_days_clicks: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(click_count), 0) FROM link_daily_clicks
         WHERE link_id = ? AND day_start_utc >= ?",
    )
    .bind(link_id)
    .bind(week_start)
    .fetch_one(pool)
    .await?;

    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT day_start_utc, click_count FROM link_daily_clicks
         WHERE link_id = ? AND day_start_utc >= ?
         ORDER BY day_start_utc ASC",
    )
    .bind(link_id)
    .bind(series_start)
    .fetch_all(pool)
    .await?;

    let mut daily = Vec::with_capacity(days as usize);
    let mut cursor = 0;
    // Rows arrive ordered; walk the full calendar range and zero-fill gaps.
    for offset in 0..days {
        let day = series_start + offset * MILLIS_PER_DAY;
        let mut count = 0;
        while cursor < rows.len() && rows[cursor].0 < day {
            cursor += 1;
        }
        if cursor < rows.len() && rows[cursor].0 == day {
            count = rows[cursor].1;
            cursor += 1;
        }
        daily.push(DailyClickPoint {
            day_start_utc: day,
            click_count: count,
        });
    }

    Ok(LinkActivitySummary {
        total_clicks,
        last_7_days_clicks,
        last_clicked_at,
        daily,
    })
}
