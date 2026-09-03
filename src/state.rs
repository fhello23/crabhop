use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;
use std::time::Duration;

use crate::config::Config;
use crate::db::analytics::ClickRecorder;

#[derive(Debug, Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    pub analytics: ClickRecorder,
}

impl AppState {
    pub fn new(db: SqlitePool, config: Arc<Config>) -> Self {
        let analytics = ClickRecorder::start(&db);
        Self {
            db,
            config,
            analytics,
        }
    }

    pub fn short_url(&self, slug: &str) -> String {
        let base = self.config.base_url.as_str().trim_end_matches('/');
        format!("{base}/{slug}")
    }
}

pub async fn connect_db(database_url: &str) -> anyhow::Result<SqlitePool> {
    let mut opts = SqliteConnectOptions::from_str(database_url)
        .map_err(|e| anyhow::anyhow!("invalid DATABASE_URL: {e}"))?;
    opts = opts
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
        .create_if_missing(true);

    // For in-memory SQLite used in tests, keep a single connection so all
    // queries share the same database.
    let is_memory = database_url.contains(":memory:");
    let max_conn = if is_memory { 1 } else { 8 };

    let pool = SqlitePoolOptions::new()
        .max_connections(max_conn)
        .connect_with(opts)
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect to SQLite: {e}"))?;

    // Enforce pragmas on every pooled connection (pool may open several).
    // `connect_with` applies them to the initial connection; belt-and-braces
    // for the rest via a quick per-pool setting on acquire is handled by
    // SqliteConnectOptions above. Re-assert here for the checked-out conn.
    sqlx::query("PRAGMA journal_mode=WAL;")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA foreign_keys=ON;")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA busy_timeout=5000;")
        .execute(&pool)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}

/// Current time as Unix milliseconds (UTC).
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Format Unix millis as RFC 3339 UTC for JSON/UI output.
pub fn millis_to_rfc3339(millis: i64) -> String {
    let secs = millis.div_euclid(1000);
    let ms_rem = millis.rem_euclid(1000);
    let dt =
        time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let dt = dt + time::Duration::milliseconds(ms_rem);
    dt.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Parse RFC 3339 (or a plain Unix-millis integer string) into Unix millis.
pub fn parse_expires_at(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(millis) = s.parse::<i64>() {
        if millis > 0 {
            return Some(millis);
        }
        return None;
    }
    let parsed =
        time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    Some(parsed.unix_timestamp() * 1000 + (parsed.millisecond() as i64))
}
