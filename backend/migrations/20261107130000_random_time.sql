-- Random takes a clock as Shuffle does (docs/specs/clip-graphs.md, "Clocks
-- of presses, one live event, and a random's clock"). Every saved graph's
-- `random` node loses its `every` and reads a new time node with that
-- every through its `time` input: `random(every=4)` becomes
-- `random(time=time(every=4))`, which plays the same (a clock of one live
-- event at a time carries no event axis, so it composes as before). A
-- random with no every took 4 beats. The time node is the lowest free
-- `time<n>`, one per random, in id order. Node ids stay, so each random
-- keeps its draw.
--
-- Graphs live in `clips.graph_json`, `perform_patterns.graph_json` and, one
-- per clip, in a draft's `base_json` and `state_json`. A node is any
-- object under a `nodes` key; each row folds over its random nodes one
-- path at a time.
--
-- Symmetric with supabase/migrations/20261107130000_random_time.sql.

CREATE TEMP TABLE random_time_docs (
    tbl TEXT NOT NULL,
    col TEXT NOT NULL,
    id TEXT NOT NULL,
    doc TEXT NOT NULL
);

INSERT INTO random_time_docs SELECT 'clips', 'graph_json', id, graph_json FROM clips
WHERE graph_json LIKE '%"random"%';
INSERT INTO random_time_docs SELECT 'perform_patterns', 'graph_json', id, graph_json
FROM perform_patterns WHERE graph_json LIKE '%"random"%';
INSERT INTO random_time_docs SELECT 'drafts', 'base_json', id, base_json FROM drafts
WHERE base_json LIKE '%"random"%';
INSERT INTO random_time_docs SELECT 'drafts', 'state_json', id, state_json FROM drafts
WHERE state_json LIKE '%"random"%';

CREATE TEMP TABLE random_time_done AS
WITH RECURSIVE
numbers(n) AS (
    SELECT 1
    UNION ALL
    SELECT n + 1 FROM numbers WHERE n < 64
),
targets(tbl, col, id, step, nodes, random) AS (
    SELECT d.tbl, d.col, d.id,
           row_number() OVER (PARTITION BY d.tbl, d.col, d.id ORDER BY t.fullkey),
           t.path, t.fullkey
    FROM random_time_docs d, json_tree(d.doc) t
    WHERE t.type = 'object'
      AND t.path LIKE '%.nodes'
      AND json_extract(t.value, '$.kind') = 'random'
),
fold(tbl, col, id, step, doc) AS (
    SELECT d.tbl, d.col, d.id, 0, d.doc
    FROM random_time_docs d
    WHERE EXISTS (SELECT 1 FROM targets t WHERE (t.tbl, t.col, t.id) = (d.tbl, d.col, d.id))
    UNION ALL
    SELECT f.tbl, f.col, f.id, f.step + 1,
           (SELECT json_set(
                       json_remove(f.doc, t.random || '.inputs.every'),
                       t.nodes || '.' || clock.name,
                       json_object('kind', 'time', 'inputs', json_object(
                           'every', coalesce(json(f.doc -> (t.random || '.inputs.every')), 4.0))),
                       t.random || '.inputs.time', json_object('node', clock.name))
            FROM (SELECT 'time' || n AS name FROM numbers
                  WHERE json_type(f.doc, t.nodes || '.time' || n) IS NULL
                  ORDER BY n LIMIT 1) clock)
    FROM fold f
    JOIN targets t ON (t.tbl, t.col, t.id) = (f.tbl, f.col, f.id) AND t.step = f.step + 1
)
SELECT tbl, col, id, doc
FROM fold f
WHERE step = (SELECT max(step) FROM targets t WHERE (t.tbl, t.col, t.id) = (f.tbl, f.col, f.id));

UPDATE clips SET graph_json = (
    SELECT doc FROM random_time_done
    WHERE tbl = 'clips' AND col = 'graph_json' AND random_time_done.id = clips.id
)
WHERE id IN (SELECT id FROM random_time_done WHERE tbl = 'clips' AND col = 'graph_json');

UPDATE perform_patterns SET graph_json = (
    SELECT doc FROM random_time_done
    WHERE tbl = 'perform_patterns' AND col = 'graph_json'
      AND random_time_done.id = perform_patterns.id
)
WHERE id IN (
    SELECT id FROM random_time_done WHERE tbl = 'perform_patterns' AND col = 'graph_json'
);

UPDATE drafts SET base_json = (
    SELECT doc FROM random_time_done
    WHERE tbl = 'drafts' AND col = 'base_json' AND random_time_done.id = drafts.id
)
WHERE id IN (SELECT id FROM random_time_done WHERE tbl = 'drafts' AND col = 'base_json');

UPDATE drafts SET state_json = (
    SELECT doc FROM random_time_done
    WHERE tbl = 'drafts' AND col = 'state_json' AND random_time_done.id = drafts.id
)
WHERE id IN (SELECT id FROM random_time_done WHERE tbl = 'drafts' AND col = 'state_json');

DROP TABLE random_time_docs;
DROP TABLE random_time_done;
