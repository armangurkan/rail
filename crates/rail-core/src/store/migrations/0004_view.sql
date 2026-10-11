-- Rail schema v4: what the mail screen reads. Additive only.
-- links: the addresses a body's [n] markers stand for, as a JSON array of strings (link n is
-- element n - 1); NULL when the body has none or was stored before this version.
ALTER TABLE messages ADD COLUMN links TEXT;
-- The unified inbox lists every account's threads newest first; keyset pages walk this.
CREATE INDEX threads_by_date ON threads(last_date DESC, id DESC);
