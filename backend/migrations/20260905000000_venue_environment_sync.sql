-- The column already exists locally. Preserve existing authored lighting on
-- first contact with the remote column's default by making those rows dirty.
-- SQLx applies this migration transactionally. Restore admission exactly as
-- found; the maintenance window only changes delivery metadata.
CREATE TEMP TABLE environment_sync_admission AS
    SELECT accepting, maintenance, remote_writes FROM auth_write_admission WHERE singleton = 1;
UPDATE auth_write_admission SET accepting = 0, maintenance = 1, remote_writes = 0 WHERE singleton = 1;
UPDATE venues SET synced_at = NULL, version = version + 1
WHERE uid IS NOT NULL AND role = 'owner'
  AND environment <> '{"mode":"indoor","houseLevel":1.0}';
UPDATE auth_write_admission SET
    accepting = (SELECT accepting FROM environment_sync_admission),
    maintenance = (SELECT maintenance FROM environment_sync_admission),
    remote_writes = (SELECT remote_writes FROM environment_sync_admission)
WHERE singleton = 1;
DROP TABLE environment_sync_admission;
