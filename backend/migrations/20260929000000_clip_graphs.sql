-- Every clip owns one graph (docs/specs/clip-graphs.md, section 4.2). The
-- graph JSON and the clip's name get their own columns. The form columns
-- `graph` and `inputs_json` stay until the one-time converter has filled
-- the new ones and the upload is verified; a later migration drops them.
ALTER TABLE clips ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE clips ADD COLUMN graph_json TEXT NOT NULL DEFAULT '{}';
