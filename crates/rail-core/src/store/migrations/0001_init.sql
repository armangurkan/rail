-- Rail schema v1. See docs/superpowers/specs/2026-10-09-rail-design.md §4.

CREATE TABLE accounts (
  id            INTEGER PRIMARY KEY,
  name          TEXT NOT NULL UNIQUE,
  kind          TEXT NOT NULL CHECK (kind IN ('gmail', 'm365')),
  address       TEXT NOT NULL UNIQUE,
  display_name  TEXT,
  color         TEXT,
  sync_cursor   TEXT,
  last_sync_at  INTEGER,
  created_at    INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE folders (
  id            INTEGER PRIMARY KEY,
  account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  remote_id     TEXT NOT NULL,
  name          TEXT NOT NULL,
  kind          TEXT NOT NULL CHECK (kind IN ('inbox','sent','drafts','trash','spam','archive','label','user')),
  parent_id     INTEGER REFERENCES folders(id) ON DELETE SET NULL,
  sync_cursor   TEXT,
  unread_count  INTEGER NOT NULL DEFAULT 0,
  total_count   INTEGER NOT NULL DEFAULT 0,
  UNIQUE (account_id, remote_id)
);

CREATE TABLE threads (
  id               INTEGER PRIMARY KEY,
  account_id       INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  remote_id        TEXT NOT NULL,
  subject          TEXT NOT NULL DEFAULT '',
  participants     TEXT NOT NULL DEFAULT '[]',
  last_date        INTEGER NOT NULL DEFAULT 0,
  message_count    INTEGER NOT NULL DEFAULT 0,
  unread_count     INTEGER NOT NULL DEFAULT 0,
  is_starred       INTEGER NOT NULL DEFAULT 0,
  has_attachments  INTEGER NOT NULL DEFAULT 0,
  UNIQUE (account_id, remote_id)
);
CREATE INDEX threads_by_last_date ON threads(account_id, last_date DESC);

CREATE TABLE messages (
  id               INTEGER PRIMARY KEY,
  account_id       INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  thread_id        INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  remote_id        TEXT NOT NULL,
  message_id       TEXT,
  in_reply_to      TEXT,
  references_ids   TEXT,
  subject          TEXT NOT NULL DEFAULT '',
  from_json        TEXT NOT NULL DEFAULT '[]',
  to_json          TEXT NOT NULL DEFAULT '[]',
  cc_json          TEXT NOT NULL DEFAULT '[]',
  bcc_json         TEXT NOT NULL DEFAULT '[]',
  date             INTEGER NOT NULL DEFAULT 0,
  snippet          TEXT NOT NULL DEFAULT '',
  is_read          INTEGER NOT NULL DEFAULT 0,
  is_starred       INTEGER NOT NULL DEFAULT 0,
  is_draft         INTEGER NOT NULL DEFAULT 0,
  size             INTEGER NOT NULL DEFAULT 0,
  body_text        TEXT,
  body_html        TEXT,
  body_fetched_at  INTEGER,
  UNIQUE (account_id, remote_id)
);
CREATE INDEX messages_by_thread ON messages(thread_id, date);
CREATE INDEX messages_by_message_id ON messages(message_id);

CREATE TABLE message_folders (
  message_id  INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  folder_id   INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  PRIMARY KEY (message_id, folder_id)
);
CREATE INDEX message_folders_by_folder ON message_folders(folder_id);

CREATE TABLE attachments (
  id              INTEGER PRIMARY KEY,
  message_id      INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
  remote_part_id  TEXT NOT NULL,
  filename        TEXT NOT NULL DEFAULT '',
  mime_type       TEXT NOT NULL DEFAULT 'application/octet-stream',
  size            INTEGER NOT NULL DEFAULT 0,
  local_path      TEXT
);

CREATE TABLE addresses (
  address    TEXT PRIMARY KEY,
  name       TEXT,
  hits       INTEGER NOT NULL DEFAULT 1,
  last_seen  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE outbox (
  id            INTEGER PRIMARY KEY,
  account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK (kind IN ('set_flags','move','trash','send','save_draft')),
  payload       TEXT NOT NULL,
  state         TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending','inflight','failed')),
  attempts      INTEGER NOT NULL DEFAULT 0,
  last_error    TEXT,
  client_token  TEXT NOT NULL UNIQUE,
  created_at    INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX outbox_by_state ON outbox(account_id, state, id);

-- External-content FTS: column names must match the messages table.
CREATE VIRTUAL TABLE messages_fts USING fts5(
  subject, from_json, to_json, body_text,
  content='messages', content_rowid='id'
);

CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, subject, from_json, to_json, body_text)
  VALUES (new.id, new.subject, new.from_json, new.to_json, new.body_text);
END;

CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_json, to_json, body_text)
  VALUES ('delete', old.id, old.subject, old.from_json, old.to_json, old.body_text);
END;

CREATE TRIGGER messages_fts_update AFTER UPDATE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_json, to_json, body_text)
  VALUES ('delete', old.id, old.subject, old.from_json, old.to_json, old.body_text);
  INSERT INTO messages_fts(rowid, subject, from_json, to_json, body_text)
  VALUES (new.id, new.subject, new.from_json, new.to_json, new.body_text);
END;
