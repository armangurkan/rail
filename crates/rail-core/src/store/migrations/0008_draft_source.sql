-- Rail schema v8: the provider id of the message a reply or forward answers, so a provider
-- that makes a reply on its original (Microsoft Graph's createReply) can name it. Additive only;
-- NULL for a new message, an imported draft and every draft saved before this version.
ALTER TABLE drafts ADD COLUMN source_remote_id TEXT;

-- Whether an attachment is a file Rail fetches and attaches again (a forwarded original, an
-- imported draft's file): every Gmail part, and on Graph a file attachment that is not inline.
-- Graph's inline pictures, attached messages and links stay where Outlook keeps them.
ALTER TABLE attachments ADD COLUMN plain_file INTEGER NOT NULL DEFAULT 1;

-- Microsoft 365 attachments stored before this version do not say which they are: their
-- messages' bodies are marked unfetched so the next fetch lists them again. The text stays.
UPDATE messages SET body_fetched_at = NULL
 WHERE account_id IN (SELECT id FROM accounts WHERE kind = 'm365')
   AND id IN (SELECT message_id FROM attachments);
