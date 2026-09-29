-- Every clip owns one graph (docs/specs/clip-graphs.md, section 4.2). The
-- graph JSON and the clip's name get their own columns, symmetric with
-- backend/migrations/20260929000000_clip_graphs.sql. The form columns
-- `graph` and `inputs_json` stay until the converted rows are uploaded and
-- verified; 20260930000000_drop_form_columns.sql drops them.
--
-- The backup tables keep the pre-conversion rows. `deploy/sync-rules.yaml`
-- selects `*` from clips, so its text does not change, but redeploy it after
-- this migration so PowerSync picks up the new columns.

begin;

alter table public.clips add column name text not null default '',
                         add column graph_json text not null default '{}';

create schema if not exists backup;
create table backup.clips_pre_clip_graphs_20260929 as select * from public.clips;
create table backup.drafts_pre_clip_graphs_20260929 as select * from public.drafts;

commit;

notify pgrst, 'reload schema';
