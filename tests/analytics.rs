mod common;

use common::setup;
use shortener::db::analytics::{
    day_start_utc, get_link_activity, link_stats, record_click, MILLIS_PER_DAY,
};

/// 2024-01-01T00:00:00Z, a UTC midnight used to pin day-boundary tests.
const MIDNIGHT: i64 = 1_704_067_200_000;

async fn test_link(state: &shortener::state::AppState, slug: &str) -> String {
    common::create_link(state, Some(slug), "https://example.com/analytics", None).await;
    let link = shortener::db::links::get_link(&state.db, slug)
        .await
        .unwrap();
    link.id
}

#[tokio::test]
async fn same_day_clicks_share_one_aggregate_row() {
    let app = setup().await;
    let id = test_link(&app.state, "sameday").await;

    for offset in [0, 1_000, 3_600_000] {
        record_click(&app.state.db, &id, MIDNIGHT + offset)
            .await
            .unwrap();
    }

    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM link_daily_clicks WHERE link_id = ?")
        .bind(&id)
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    let (total, last) = link_stats(&app.state.db, &id).await.unwrap();
    assert_eq!(total, 3);
    assert_eq!(last, Some(MIDNIGHT + 3_600_000));
}

#[tokio::test]
async fn clicks_across_utc_midnight_split_into_two_rows() {
    let app = setup().await;
    let id = test_link(&app.state, "boundary").await;

    record_click(&app.state.db, &id, MIDNIGHT - 1)
        .await
        .unwrap();
    record_click(&app.state.db, &id, MIDNIGHT).await.unwrap();

    let days: Vec<i64> = sqlx::query_scalar(
        "SELECT day_start_utc FROM link_daily_clicks WHERE link_id = ? ORDER BY day_start_utc",
    )
    .bind(&id)
    .fetch_all(&app.state.db)
    .await
    .unwrap();
    assert_eq!(days, vec![MIDNIGHT - MILLIS_PER_DAY, MIDNIGHT]);
    assert_eq!(day_start_utc(MIDNIGHT - 1), MIDNIGHT - MILLIS_PER_DAY);
    assert_eq!(day_start_utc(MIDNIGHT), MIDNIGHT);
}

#[tokio::test]
async fn activity_series_is_complete_and_zero_filled() {
    let app = setup().await;
    let id = test_link(&app.state, "series").await;

    // Clicks 29 days ago, yesterday (twice), and an hour ago today.
    let now = MIDNIGHT + 29 * MILLIS_PER_DAY + 3_600_000;
    record_click(&app.state.db, &id, MIDNIGHT).await.unwrap();
    record_click(&app.state.db, &id, MIDNIGHT + 28 * MILLIS_PER_DAY)
        .await
        .unwrap();
    record_click(&app.state.db, &id, MIDNIGHT + 28 * MILLIS_PER_DAY + 5_000)
        .await
        .unwrap();
    record_click(&app.state.db, &id, now).await.unwrap();

    let summary = get_link_activity(&app.state.db, &id, 30, now)
        .await
        .unwrap();
    assert_eq!(summary.total_clicks, 4);
    assert_eq!(summary.last_clicked_at, Some(now));
    // Last seven calendar days hold yesterday's two plus today's one.
    assert_eq!(summary.last_7_days_clicks, 3);

    assert_eq!(summary.daily.len(), 30);
    for window in summary.daily.windows(2) {
        assert_eq!(
            window[1].day_start_utc - window[0].day_start_utc,
            MILLIS_PER_DAY
        );
    }
    assert_eq!(summary.daily[0].day_start_utc, MIDNIGHT);
    assert_eq!(summary.daily[0].click_count, 1);
    assert_eq!(summary.daily[28].click_count, 2);
    assert_eq!(summary.daily[29].click_count, 1);
    assert!(summary.daily[1..28].iter().all(|d| d.click_count == 0));

    // A link with no clicks reports an honest empty series.
    let quiet = test_link(&app.state, "quiet").await;
    let empty = get_link_activity(&app.state.db, &quiet, 30, now)
        .await
        .unwrap();
    assert_eq!(empty.total_clicks, 0);
    assert_eq!(empty.last_7_days_clicks, 0);
    assert_eq!(empty.last_clicked_at, None);
    assert_eq!(empty.daily.len(), 30);
    assert!(empty.daily.iter().all(|d| d.click_count == 0));
}
