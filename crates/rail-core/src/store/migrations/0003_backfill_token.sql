-- Rail schema v3: a backfill resumes from Gmail's page token. Additive only.
-- Gmail lists only roughly newest first, so a date marker alone cannot say what a resumed
-- listing has yet to return. The token is only valid for the query it was issued for, so that
-- query's bounds are kept with it. All four are set together, and with the page they follow.
-- Times: *_ms are milliseconds (internalDate), backfill_token_at is Unix seconds.

-- Gmail's nextPageToken after the last committed backfill page; NULL when none waits.
ALTER TABLE accounts ADD COLUMN backfill_page_token TEXT;
-- The listing the token belongs to: its after: and before: bounds (NULL: unbounded).
ALTER TABLE accounts ADD COLUMN backfill_list_after_ms INTEGER;
ALTER TABLE accounts ADD COLUMN backfill_list_before_ms INTEGER;
-- When Gmail issued the token.
ALTER TABLE accounts ADD COLUMN backfill_token_at INTEGER;
