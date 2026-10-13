-- Rail schema v8: the provider id of the message a reply or forward answers, so a provider
-- that makes a reply on its original (Microsoft Graph's createReply) can name it. Additive only;
-- NULL for a new message, an imported draft and every draft saved before this version.
ALTER TABLE drafts ADD COLUMN source_remote_id TEXT;
