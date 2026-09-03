CREATE TABLE links (
    id            TEXT NOT NULL UNIQUE,
    slug          TEXT NOT NULL PRIMARY KEY,
    target_url    TEXT NOT NULL,
    label         TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    expires_at    INTEGER,
    disabled_at   INTEGER
);

CREATE INDEX links_created_at_idx ON links(created_at DESC);
CREATE INDEX links_expires_at_idx ON links(expires_at)
    WHERE expires_at IS NOT NULL;
