CREATE TABLE link_daily_clicks (
    link_id         TEXT NOT NULL REFERENCES links(id) ON DELETE CASCADE,
    day_start_utc   INTEGER NOT NULL,
    click_count     INTEGER NOT NULL DEFAULT 0 CHECK (click_count >= 0),
    last_clicked_at INTEGER NOT NULL,
    PRIMARY KEY (link_id, day_start_utc)
);
