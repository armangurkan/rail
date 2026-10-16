ALTER TABLE outbox ADD COLUMN folder_remotes TEXT NOT NULL DEFAULT '{}';

-- Preserve identities before a replay replaces the reversal's local folder ids. Older
-- actions without recorded identities can only use a folder still in their own account.
WITH requested AS (
  SELECT o.id AS row_id, o.account_id, f.value AS folder
  FROM outbox o, json_each(o.payload, '$.add') f WHERE json_extract(o.payload, '$.kind') = 'labels'
  UNION
  SELECT o.id, o.account_id, f.value
  FROM outbox o, json_each(o.payload, '$.remove') f WHERE json_extract(o.payload, '$.kind') = 'labels'
  UNION
  SELECT id, account_id, json_extract(payload, '$.to') FROM outbox WHERE json_extract(payload, '$.kind') = 'move'
), identities AS (
  SELECT r.row_id, r.folder, COALESCE(
    (SELECT f.value FROM outbox o, json_each(o.reversal) p,
       json_each(p.value, '$.remotes') f
     WHERE o.id = r.row_id AND CAST(f.key AS INTEGER) = r.folder LIMIT 1),
    (SELECT remote_id FROM folders WHERE account_id = r.account_id AND id = r.folder)
  ) AS remote FROM requested r
)
UPDATE outbox SET folder_remotes = (
  SELECT json_group_object(CAST(folder AS TEXT), remote) FROM identities
  WHERE row_id = outbox.id AND remote IS NOT NULL
);
