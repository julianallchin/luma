-- Python figures live in Supabase Storage (`agent-figures`), not in
-- `agent_thread_messages.parts_json`: a stored figure names its object by
-- `path` and the PNG sits in the local cache until the media loop uploads it.
--
-- Local only: this device's queue of figures it has cached and not yet
-- uploaded. A row goes when its upload lands, so an offline run uploads on the
-- next pass that has a session.
CREATE TABLE agent_figure_uploads (
    path TEXT PRIMARY KEY NOT NULL
);
