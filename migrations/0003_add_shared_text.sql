-- Keep existing redirect rows and analytics intact. Text shares use an empty
-- target_url and a non-null text_content; a slug always identifies one resource.
ALTER TABLE links ADD COLUMN text_content TEXT
    CHECK (text_content IS NULL OR (target_url = '' AND length(CAST(text_content AS BLOB)) BETWEEN 1 AND 65536));
