-- Rail schema v6: the outbox carries actions on threads. Additive only.
-- not_before: Unix seconds before which the worker leaves the row alone (the undo grace, then
-- the retry backoff). thread_ids: JSON array of local threads.id. inflight_at: when the worker
-- claimed the row (a row still inflight at daemon start is sent again). reversal: JSON array of
-- what the local change replaced, per message (ids, flags and folder ids only), so an undo or a
-- permanent failure restores it exactly. kind keeps v1's CHECK; payload holds the action.

ALTER TABLE outbox ADD COLUMN not_before INTEGER NOT NULL DEFAULT 0;
ALTER TABLE outbox ADD COLUMN thread_ids TEXT NOT NULL DEFAULT '[]';
ALTER TABLE outbox ADD COLUMN inflight_at INTEGER;
ALTER TABLE outbox ADD COLUMN reversal TEXT NOT NULL DEFAULT '[]';

CREATE INDEX outbox_by_due ON outbox(account_id, state, not_before, id);
