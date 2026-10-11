-- Rail schema v5: the full-text index holds senders and recipients as plain text.
-- v1 indexed from_json and to_json, so the JSON keys "name" and "address" and the word "null"
-- matched every message. from_text and to_text hold "Name address" per mailbox, separated by
-- spaces (only the address when the name is missing or empty); to_text covers To, Cc and Bcc
-- in that order. The writer fills both with store::sync_write::address_text; the backfill
-- below gives the same text. Unreadable JSON and entries that are not objects add nothing.

ALTER TABLE messages ADD COLUMN from_text TEXT NOT NULL DEFAULT '';
ALTER TABLE messages ADD COLUMN to_text TEXT NOT NULL DEFAULT '';

DROP TRIGGER messages_fts_insert;
DROP TRIGGER messages_fts_delete;
DROP TRIGGER messages_fts_update;
DROP TABLE messages_fts;

UPDATE messages SET
  from_text = COALESCE((
    SELECT group_concat(
      CASE WHEN COALESCE(e.value ->> '$.name', '') = '' THEN e.value ->> '$.address'
           ELSE (e.value ->> '$.name') || ' ' || (e.value ->> '$.address') END,
      ' ' ORDER BY e.key)
    FROM json_each(CASE WHEN json_valid(from_json) THEN from_json ELSE '[]' END) e
    WHERE e.type = 'object'), ''),
  to_text = COALESCE((
    SELECT group_concat(
      CASE WHEN COALESCE(e.value ->> '$.name', '') = '' THEN e.value ->> '$.address'
           ELSE (e.value ->> '$.name') || ' ' || (e.value ->> '$.address') END,
      ' ' ORDER BY e.part, e.key)
    FROM (
      SELECT 0 AS part, key, value, type
        FROM json_each(CASE WHEN json_valid(to_json) THEN to_json ELSE '[]' END)
      UNION ALL
      SELECT 1, key, value, type
        FROM json_each(CASE WHEN json_valid(cc_json) THEN cc_json ELSE '[]' END)
      UNION ALL
      SELECT 2, key, value, type
        FROM json_each(CASE WHEN json_valid(bcc_json) THEN bcc_json ELSE '[]' END)
    ) e
    WHERE e.type = 'object'), '');

-- External-content FTS: column names must match the messages table.
CREATE VIRTUAL TABLE messages_fts USING fts5(
  subject, from_text, to_text, body_text,
  content='messages', content_rowid='id'
);

CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, subject, from_text, to_text, body_text)
  VALUES (new.id, new.subject, new.from_text, new.to_text, new.body_text);
END;

CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_text, to_text, body_text)
  VALUES ('delete', old.id, old.subject, old.from_text, old.to_text, old.body_text);
END;

-- Only indexed columns re-index; flag and folder changes skip the FTS work.
CREATE TRIGGER messages_fts_update
AFTER UPDATE OF subject, from_text, to_text, body_text ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, subject, from_text, to_text, body_text)
  VALUES ('delete', old.id, old.subject, old.from_text, old.to_text, old.body_text);
  INSERT INTO messages_fts(rowid, subject, from_text, to_text, body_text)
  VALUES (new.id, new.subject, new.from_text, new.to_text, new.body_text);
END;

-- Index every existing row from the content table (one-time cost on the first open).
INSERT INTO messages_fts(messages_fts) VALUES ('rebuild');
