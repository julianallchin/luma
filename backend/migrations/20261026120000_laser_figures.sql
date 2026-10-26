-- Laser figures (docs/design/lasers.md, "Figures"): a laser node draws the
-- figures wired into its `figures`, so one clip mixes beams, lines and
-- rings. Every saved laser node splits in two: a `figure` node takes its
-- `kind` setting and its heads, count, sides, x, y, size, rotation and
-- color, and the laser keeps its zone and alpha and draws that one figure.
-- The figure is `figure1`, or `<laser id>_figure` where `figure1` is taken.
--
-- Graphs live in `clips.graph_json` and, one per clip, in a draft's
-- `base_json` and `state_json`. A node is any object under a `nodes` key;
-- each row folds over its laser nodes one path at a time.
--
-- Symmetric with supabase/migrations/20261026120000_laser_figures.sql.

CREATE TEMP TABLE laser_figure_docs (
    tbl TEXT NOT NULL,
    col TEXT NOT NULL,
    id TEXT NOT NULL,
    doc TEXT NOT NULL
);

INSERT INTO laser_figure_docs SELECT 'clips', 'graph_json', id, graph_json FROM clips
WHERE graph_json LIKE '%"laser"%';
INSERT INTO laser_figure_docs SELECT 'drafts', 'base_json', id, base_json FROM drafts
WHERE base_json LIKE '%"laser"%';
INSERT INTO laser_figure_docs SELECT 'drafts', 'state_json', id, state_json FROM drafts
WHERE state_json LIKE '%"laser"%';

CREATE TEMP TABLE laser_figure_done AS
WITH RECURSIVE
targets(tbl, col, id, step, figure_path, figure, laser, laser_path) AS (
    SELECT tbl, col, id,
           row_number() OVER (PARTITION BY tbl, col, id ORDER BY fullkey),
           path || '.' || fid,
           json_object(
               'kind', 'figure',
               'settings', json_object('kind', coalesce(value ->> '$.settings.kind', 'beams')),
               'inputs', json(json_remove(coalesce(value -> '$.inputs', '{}'), '$.alpha'))),
           json_object(
               'kind', 'laser',
               'settings', json_object('zone', coalesce(value ->> '$.settings.zone', '')),
               'inputs', CASE
                   WHEN json_type(value, '$.inputs.alpha') IS NULL
                   THEN json_object('figures', json_array(json_object('node', fid)))
                   ELSE json_object('figures', json_array(json_object('node', fid)),
                                    'alpha', json(value -> '$.inputs.alpha'))
               END),
           fullkey
    FROM (
        SELECT d.tbl, d.col, d.id, t.path, t.fullkey, t.value,
               CASE WHEN json_type(d.doc, t.path || '.figure1') IS NULL
                    THEN 'figure1' ELSE t.key || '_figure' END AS fid
        FROM laser_figure_docs d, json_tree(d.doc) t
        WHERE t.type = 'object'
          AND t.path LIKE '%.nodes'
          AND json_extract(t.value, '$.kind') = 'laser'
          AND json_type(t.value, '$.inputs.figures') IS NULL
    )
),
fold(tbl, col, id, step, doc) AS (
    SELECT d.tbl, d.col, d.id, 0, d.doc
    FROM laser_figure_docs d
    WHERE EXISTS (SELECT 1 FROM targets t WHERE (t.tbl, t.col, t.id) = (d.tbl, d.col, d.id))
    UNION ALL
    SELECT f.tbl, f.col, f.id, f.step + 1,
           json_set(f.doc, t.figure_path, json(t.figure), t.laser_path, json(t.laser))
    FROM fold f
    JOIN targets t ON (t.tbl, t.col, t.id) = (f.tbl, f.col, f.id) AND t.step = f.step + 1
)
SELECT tbl, col, id, doc
FROM fold f
WHERE step = (SELECT max(step) FROM targets t WHERE (t.tbl, t.col, t.id) = (f.tbl, f.col, f.id));

UPDATE clips SET graph_json = (
    SELECT doc FROM laser_figure_done
    WHERE tbl = 'clips' AND col = 'graph_json' AND laser_figure_done.id = clips.id
)
WHERE id IN (SELECT id FROM laser_figure_done WHERE tbl = 'clips' AND col = 'graph_json');

UPDATE drafts SET base_json = (
    SELECT doc FROM laser_figure_done
    WHERE tbl = 'drafts' AND col = 'base_json' AND laser_figure_done.id = drafts.id
)
WHERE id IN (SELECT id FROM laser_figure_done WHERE tbl = 'drafts' AND col = 'base_json');

UPDATE drafts SET state_json = (
    SELECT doc FROM laser_figure_done
    WHERE tbl = 'drafts' AND col = 'state_json' AND laser_figure_done.id = drafts.id
)
WHERE id IN (SELECT id FROM laser_figure_done WHERE tbl = 'drafts' AND col = 'state_json');

DROP TABLE laser_figure_docs;
DROP TABLE laser_figure_done;
