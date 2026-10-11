-- Rail schema v2: sync state. Additive only: it adds tables and columns and changes
-- nothing that v1 created.
-- Times: *_at are Unix seconds; internal_date, backfill_before and backfill_window are
-- Gmail's internalDate in milliseconds. accounts.last_sync_at already exists (0001).

ALTER TABLE accounts ADD COLUMN sync_phase TEXT NOT NULL DEFAULT 'idle';
ALTER TABLE accounts ADD COLUMN last_error_code TEXT;
ALTER TABLE accounts ADD COLUMN last_error_at INTEGER;
ALTER TABLE accounts ADD COLUMN needs_auth INTEGER NOT NULL DEFAULT 0;
-- The oldest internal date a running backfill has committed; NULL when none runs.
ALTER TABLE accounts ADD COLUMN backfill_before INTEGER;
-- The historyId read before the running backfill began; becomes sync_cursor at its end.
ALTER TABLE accounts ADD COLUMN backfill_cursor TEXT;
-- The window start the last complete backfill covered (0: the whole mailbox).
ALTER TABLE accounts ADD COLUMN backfill_window INTEGER;
-- When the running full backfill began, for the sweep of mail it did not see again.
ALTER TABLE accounts ADD COLUMN backfill_started_at INTEGER;

ALTER TABLE messages ADD COLUMN origin TEXT NOT NULL DEFAULT 'sync'
  CHECK (origin IN ('sync', 'search'));
ALTER TABLE messages ADD COLUMN searched_at INTEGER;
ALTER TABLE messages ADD COLUMN internal_date INTEGER NOT NULL DEFAULT 0;
-- Rows from before this migration: `date` is Unix seconds, so keep them out of the 'ancient' bucket.
UPDATE messages SET internal_date = date * 1000 WHERE internal_date = 0;
ALTER TABLE messages ADD COLUMN synced_at INTEGER;

CREATE INDEX messages_by_origin_date ON messages(account_id, origin, internal_date);
CREATE INDEX messages_by_origin_searched ON messages(account_id, origin, searched_at);
CREATE INDEX messages_without_body ON messages(account_id, thread_id)
  WHERE body_fetched_at IS NULL;
CREATE INDEX messages_by_thread_internal ON messages(thread_id, internal_date);
CREATE INDEX messages_by_account_date ON messages(account_id, internal_date);
