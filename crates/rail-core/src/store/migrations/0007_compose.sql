-- Rail schema v7: drafts, a stored Reply-To, and outbox rows keyed by draft. Additive only.

CREATE TABLE drafts (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
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
  -- When Rail first handed this draft to Gmail's send: from then on, every send of it looks
  -- the Message-ID up first (a failure after Gmail accepted it must not send it twice).
  send_attempted_at INTEGER,
  -- The client token of the draft.save that created it: the same create sent again answers
  -- this draft instead of making a second one.
  client_token      TEXT UNIQUE,
  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL
);
CREATE INDEX drafts_by_account ON drafts(account_id, updated_at);

ALTER TABLE messages ADD COLUMN reply_to_json TEXT NOT NULL DEFAULT '[]';

-- AUTOINCREMENT above: a draft id is never reused, so a row left for a deleted draft can never
-- act on a newer one; the cascade removes such rows anyway.
ALTER TABLE outbox ADD COLUMN draft_id INTEGER REFERENCES drafts(id) ON DELETE CASCADE;
CREATE INDEX outbox_by_draft ON outbox(draft_id);
