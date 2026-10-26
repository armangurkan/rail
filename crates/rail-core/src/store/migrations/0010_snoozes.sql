-- Rail schema v10: snoozes (a thread taken out of the inbox now and put back at a time).
-- thread_remote: the provider's thread id (thread rowids are reused). until: Unix seconds.
-- archive_row: the snooze's archive while that row lives; deleting the row (an undo, a sync
-- settling its failure, a thread forgotten) deletes the snooze, and completing it records the
-- messages it took out of the inbox in snooze_messages and clears the link. woken_at: when the
-- thread went back; the rollup dates it there. A snooze's state is read from these columns.

CREATE TABLE IF NOT EXISTS snoozes (
  id            INTEGER PRIMARY KEY,
  account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  thread_remote TEXT    NOT NULL,
  until         INTEGER NOT NULL,
  archive_row   INTEGER REFERENCES outbox(id) ON DELETE CASCADE,
  woken_at      INTEGER,
  created_at    INTEGER NOT NULL,
  UNIQUE (account_id, thread_remote)
);

CREATE INDEX IF NOT EXISTS snoozes_due ON snoozes(account_id, until)
  WHERE archive_row IS NULL AND woken_at IS NULL;
CREATE INDEX IF NOT EXISTS snoozes_by_archive ON snoozes(archive_row) WHERE archive_row IS NOT NULL;

CREATE TABLE IF NOT EXISTS snooze_messages (
  snooze_id      INTEGER NOT NULL REFERENCES snoozes(id) ON DELETE CASCADE,
  message_remote TEXT    NOT NULL,
  PRIMARY KEY (snooze_id, message_remote)
);
