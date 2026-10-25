-- Random replaces Noise (docs/design/lasers.md, v2). Every saved graph's
-- `noise` node becomes a `random` node that plays the same: `period` is
-- `every`, `glide` 1 drifts the whole turn as Noise did, and `scale` and
-- `contrast` keep their meaning. Node ids stay, so each node keeps its draw.
--
-- Graphs live in `clips.graph_json` and, one per clip, in a draft's
-- `base_json` and `state_json`. A node is any object under a `nodes` key;
-- each row folds over its noise nodes one path at a time.
--
-- Symmetric with supabase/migrations/20261025120000_random_node.sql.

CREATE TEMP TABLE random_node_docs (
    tbl TEXT NOT NULL,
    col TEXT NOT NULL,
    id TEXT NOT NULL,
    doc TEXT NOT NULL
);

INSERT INTO random_node_docs SELECT 'clips', 'graph_json', id, graph_json FROM clips;
INSERT INTO random_node_docs SELECT 'drafts', 'base_json', id, base_json FROM drafts;
INSERT INTO random_node_docs SELECT 'drafts', 'state_json', id, state_json FROM drafts;

CREATE TEMP TABLE random_node_done AS
WITH RECURSIVE
targets(tbl, col, id, step, path) AS (
    SELECT d.tbl, d.col, d.id,
           row_number() OVER (PARTITION BY d.tbl, d.col, d.id ORDER BY t.fullkey),
           t.fullkey
    FROM random_node_docs d, json_tree(d.doc) t
    WHERE t.type = 'object'
      AND t.path LIKE '%.nodes'
      AND json_extract(t.value, '$.kind') = 'noise'
),
fold(tbl, col, id, step, doc) AS (
    SELECT d.tbl, d.col, d.id, 0, d.doc
    FROM random_node_docs d
    WHERE EXISTS (SELECT 1 FROM targets t WHERE (t.tbl, t.col, t.id) = (d.tbl, d.col, d.id))
    UNION ALL
    SELECT f.tbl, f.col, f.id, f.step + 1,
           CASE WHEN json_type(f.doc, t.path || '.inputs.period') IS NULL
                THEN json_set(f.doc,
                              t.path || '.kind', 'random',
                              t.path || '.inputs.glide', 1.0)
                ELSE json_set(json_remove(f.doc, t.path || '.inputs.period'),
                              t.path || '.kind', 'random',
                              t.path || '.inputs.every',
                              json(f.doc -> (t.path || '.inputs.period')),
                              t.path || '.inputs.glide', 1.0)
           END
    FROM fold f
    JOIN targets t ON (t.tbl, t.col, t.id) = (f.tbl, f.col, f.id) AND t.step = f.step + 1
)
SELECT tbl, col, id, doc
FROM fold f
WHERE step = (SELECT max(step) FROM targets t WHERE (t.tbl, t.col, t.id) = (f.tbl, f.col, f.id));

UPDATE clips SET graph_json = (
    SELECT doc FROM random_node_done
    WHERE tbl = 'clips' AND col = 'graph_json' AND random_node_done.id = clips.id
)
WHERE id IN (SELECT id FROM random_node_done WHERE tbl = 'clips' AND col = 'graph_json');

UPDATE drafts SET base_json = (
    SELECT doc FROM random_node_done
    WHERE tbl = 'drafts' AND col = 'base_json' AND random_node_done.id = drafts.id
)
WHERE id IN (SELECT id FROM random_node_done WHERE tbl = 'drafts' AND col = 'base_json');

UPDATE drafts SET state_json = (
    SELECT doc FROM random_node_done
    WHERE tbl = 'drafts' AND col = 'state_json' AND random_node_done.id = drafts.id
)
WHERE id IN (SELECT id FROM random_node_done WHERE tbl = 'drafts' AND col = 'state_json');

DROP TABLE random_node_docs;
DROP TABLE random_node_done;
