-- Rail schema v7: drafts, a stored Reply-To, and outbox rows keyed by draft. Additive only.

CREATE TABLE drafts (
  id                INTEGER PRIMARY KEY,
  account_id        INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  kind              TEXT NOT NULL CHECK (kind IN ('new','reply','reply_all','forward')),
  thread_id         INTEGER REFERENCES threads(id) ON DELETE SET NULL,
  remote_thread_id  TEXT,
  in_reply_to       TEXT,
  references_ids    TEXT,
  to_json           TEXT NOT NULL DEFAULT '[]',
  cc_json           TEXT NOT NULL DEFAULT '[]',
  bcc_json          TEXT NOT NULL DEFAULT '[]',
  subject           TEXT NOT NULL DEFAULT '',
  body              TEXT NOT NULL DEFAULT '',
  attachments_json  TEXT NOT NULL DEFAULT '[]',
  message_id        TEXT NOT NULL UNIQUE,
  remote_draft_id   TEXT,
  remote_message_id TEXT,
  state             TEXT NOT NULL DEFAULT 'editing' CHECK (state IN ('editing','queued')),
  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL
);
CREATE INDEX drafts_by_account ON drafts(account_id, updated_at);

ALTER TABLE messages ADD COLUMN reply_to_json TEXT NOT NULL DEFAULT '[]';

ALTER TABLE outbox ADD COLUMN draft_id INTEGER;
CREATE INDEX outbox_by_draft ON outbox(draft_id);
