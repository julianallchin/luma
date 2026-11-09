-- A refused upload is shown in the status bar as "N not saved" until the user
-- has read the list and dismissed it. The row stays: its `data_json` is what
-- recovery would replay, and dismissing is only "I have seen this".
ALTER TABLE sync_rejections ADD COLUMN dismissed_at TEXT;
