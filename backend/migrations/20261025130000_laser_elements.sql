-- Laser v2 (docs/design/lasers.md, "v2"): a laser node draws `count`
-- elements of a `kind` (beams, line, ring, polygon, grid), each a head with
-- its own x, y, size, rotation and color. The old node's `shape`, `beams`
-- and `join` go, and Color clips no longer reach projectors. Every saved
-- graph moves here, once; nothing converts at runtime.
--
-- Symmetric with supabase/migrations/20261025130000_laser_elements.sql,
-- which says what moves where and what is lost.

-- Old laser nodes: no `kind` setting.
CREATE TEMP TABLE laser_old AS
SELECT clip_id, lid, shape, zone, n, inputs, loose,
       CASE
           WHEN shape = 'grid' THEN 'grid'
           WHEN joined THEN CASE WHEN shape = 'circle' THEN 'ring' ELSE 'line' END
           ELSE 'beams'
       END AS kind
FROM (
    SELECT c.id AS clip_id,
           n.key AS lid,
           coalesce(n.value ->> '$.settings.shape', 'fan') AS shape,
           coalesce(n.value ->> '$.settings.zone', '') AS zone,
           CASE WHEN json_type(n.value, '$.inputs.beams') IN ('integer', 'real')
                THEN CAST(n.value ->> '$.inputs.beams' AS INTEGER) ELSE 8 END AS n,
           json_remove(coalesce(n.value -> '$.inputs', '{}'), '$.beams', '$.join') AS inputs,
           json_type(n.value, '$.inputs.join') IN ('integer', 'real')
               AND n.value ->> '$.inputs.join' >= 0.5 AS joined,
           json_type(n.value, '$.inputs.join') = 'object' AS loose
    FROM clips c, json_each(c.graph_json, '$.nodes') n
    WHERE n.value ->> '$.kind' = 'laser' AND json_type(n.value, '$.settings.kind') IS NULL
);

-- Axes that place each beam itself: a fan whose x is set, a circle of
-- beams. `centre` plus `size` times `reach` times `points` over the
-- element's order. Order puts element i of n at (i + 0.5) / n, so a fan's
-- curve runs ±size·n/(n − 1) to put its end beams at ±size.
CREATE TEMP TABLE laser_spread AS
SELECT clip_id, 'x' AS axis,
       '[[0, 0], [1, 1]]' AS points,
       CAST(n AS REAL) / (n - 1) AS reach,
       inputs -> '$.x' AS centre,
       coalesce(inputs -> '$.size', '0.5') AS size
FROM laser_old
WHERE kind = 'beams' AND shape = 'fan' AND json_type(inputs, '$.x') IS NOT NULL AND n > 1
UNION ALL
SELECT clip_id, 'x',
       '[[0, 0.5, "sine-out"], [0.25, 1, "sine-in"], [0.5, 0.5, "sine-out"], [0.75, 0, "sine-in"], [1, 0.5]]',
       1.0, coalesce(inputs -> '$.x', '0'), coalesce(inputs -> '$.size', '0.5')
FROM laser_old WHERE kind = 'beams' AND shape = 'circle'
UNION ALL
SELECT clip_id, 'y',
       '[[0, 1, "sine-in"], [0.25, 0.5, "sine-out"], [0.5, 0, "sine-in"], [0.75, 0.5, "sine-out"], [1, 1]]',
       1.0, coalesce(inputs -> '$.y', '0'), coalesce(inputs -> '$.size', '0.5')
FROM laser_old WHERE kind = 'beams' AND shape = 'circle';

-- The nodes each clip gains.
CREATE TEMP TABLE laser_added (clip_id TEXT, key TEXT, node TEXT);
INSERT INTO laser_added
SELECT DISTINCT clip_id, 'm_split', json_object('kind', 'split', 'settings', json_object('by', 'fixture'))
FROM laser_spread;
INSERT INTO laser_added
SELECT DISTINCT clip_id, 'm_order', json_object(
    'kind', 'space',
    'settings', json_object('kind', 'order', 'wrap', 'no'),
    'inputs', json_object('heads', json_object('node', 'm_split')))
FROM laser_spread;
-- A number at a number: one curve from centre − size·reach to centre + size·reach.
INSERT INTO laser_added
SELECT clip_id, 'm_' || axis, json_object(
    'kind', 'curve',
    'settings', json_object('kind', 'number'),
    'inputs', json_object(
        'x', json_object('node', 'm_order'),
        'shape', json_object('points', json(points)),
        'low', (centre ->> '$') - (size ->> '$') * reach,
        'high', (centre ->> '$') + (size ->> '$') * reach))
FROM laser_spread
WHERE json_type(centre) IN ('integer', 'real') AND json_type(size) IN ('integer', 'real');
-- Anything wired: a curve −reach to reach, times size, plus centre.
INSERT INTO laser_added
SELECT clip_id, 'm_' || axis || '_u', json_object(
    'kind', 'curve',
    'settings', json_object('kind', 'number'),
    'inputs', json_object(
        'x', json_object('node', 'm_order'),
        'shape', json_object('points', json(points)),
        'low', -reach,
        'high', reach))
FROM laser_spread
WHERE NOT (json_type(centre) IN ('integer', 'real') AND json_type(size) IN ('integer', 'real'));
INSERT INTO laser_added
SELECT clip_id, 'm_' || axis || '_w', json_object(
    'kind', 'math',
    'settings', json_object('op', '*'),
    'inputs', json_object('values', json_array(json_object('node', 'm_' || axis || '_u'), json(size))))
FROM laser_spread
WHERE NOT (json_type(centre) IN ('integer', 'real') AND json_type(size) IN ('integer', 'real'));
INSERT INTO laser_added
SELECT clip_id, 'm_' || axis, json_object(
    'kind', 'math',
    'settings', json_object('op', '+'),
    'inputs', json_object('values', json_array(json_object('node', 'm_' || axis || '_w'), json(centre))))
FROM laser_spread
WHERE NOT (json_type(centre) IN ('integer', 'real') AND json_type(size) IN ('integer', 'real'));

UPDATE laser_old SET inputs = json_set(inputs, '$.count', n) WHERE kind IN ('beams', 'grid');
UPDATE laser_old SET inputs = json_set(inputs, '$.x', json_object('node', 'm_x'))
WHERE clip_id IN (SELECT clip_id FROM laser_spread WHERE axis = 'x');
UPDATE laser_old SET inputs = json_set(inputs, '$.y', json_object('node', 'm_y'))
WHERE clip_id IN (SELECT clip_id FROM laser_spread WHERE axis = 'y');

UPDATE clips
SET graph_json = json_set(
    json_patch(json(clips.graph_json), json_object('nodes', json(
        (SELECT json_group_object(a.key, json(a.node)) FROM laser_added a WHERE a.clip_id = clips.id)))),
    '$.nodes.' || l.lid,
    json_object(
        'kind', 'laser',
        'settings', json_object('kind', l.kind, 'zone', l.zone),
        'inputs', json(l.inputs)))
FROM laser_old l
WHERE l.clip_id = clips.id;

-- A wired join's nodes, read by nothing now.
CREATE TEMP TABLE laser_read AS
WITH RECURSIVE reach (clip_id, key) AS (
    SELECT clip_id, lid FROM laser_old WHERE loose
    UNION
    SELECT r.clip_id, t.value
    FROM reach r
    JOIN clips c ON c.id = r.clip_id,
    json_tree(c.graph_json, '$.nodes.' || r.key) t
    WHERE t.key = 'node' AND t.type = 'text'
)
SELECT * FROM reach;
UPDATE clips
SET graph_json = json_set(clips.graph_json, '$.nodes', (
    SELECT json_group_object(n.key, json(n.value))
    FROM json_each(clips.graph_json, '$.nodes') n
    WHERE n.key IN (SELECT r.key FROM laser_read r WHERE r.clip_id = clips.id)))
WHERE clips.id IN (SELECT clip_id FROM laser_read);

-- Laser and Color clips that pair one to one.
CREATE TEMP TABLE laser_color_pairs AS
WITH lasers AS (
    SELECT * FROM clips
    WHERE EXISTS (SELECT 1 FROM json_each(clips.graph_json, '$.nodes') n WHERE n.value ->> '$.kind' = 'laser')
), colors AS (
    SELECT * FROM clips
    WHERE EXISTS (SELECT 1 FROM json_each(clips.graph_json, '$.nodes') n WHERE n.value ->> '$.kind' = 'color')
), pairs AS (
    SELECT l.id AS laser_id, c.id AS color_id
    FROM lasers l
    JOIN colors c ON c.score_id = l.score_id
        AND abs(c.start - l.start) < 0.01
        AND abs(c.duration - l.duration) < 0.01
        AND c.selection_json = l.selection_json
)
SELECT * FROM pairs p
WHERE (SELECT count(*) FROM pairs q WHERE q.laser_id = p.laser_id) = 1
  AND (SELECT count(*) FROM pairs q WHERE q.color_id = p.color_id) = 1;

-- Each Color graph's nodes with every wire renamed `c_<id>`, and its
-- colour node's inputs.
CREATE TEMP TABLE laser_fold AS
WITH RECURSIVE graphs AS (
    SELECT p.laser_id, p.color_id,
           (SELECT n.key FROM json_each(l.graph_json, '$.nodes') n WHERE n.value ->> '$.kind' = 'laser') AS lid,
           (SELECT n.key FROM json_each(c.graph_json, '$.nodes') n WHERE n.value ->> '$.kind' = 'color') AS cid,
           json(json(c.graph_json) -> '$.nodes') AS nodes
    FROM laser_color_pairs p
    JOIN clips l ON l.id = p.laser_id
    JOIN clips c ON c.id = p.color_id
), ids AS (
    SELECT g.color_id, n.key, row_number() OVER (PARTITION BY g.color_id ORDER BY n.key) AS k
    FROM graphs g, json_each(g.nodes) n
), walk (color_id, k, nodes) AS (
    SELECT color_id, 0, nodes FROM graphs
    UNION ALL
    SELECT w.color_id, w.k + 1,
           replace(w.nodes, '{"node":"' || i.key || '"}', '{"node":"c_' || i.key || '"}')
    FROM walk w JOIN ids i ON i.color_id = w.color_id AND i.k = w.k + 1
), renamed AS (
    SELECT color_id, nodes FROM walk w
    WHERE k = (SELECT max(v.k) FROM walk v WHERE v.color_id = w.color_id)
)
SELECT g.laser_id, g.color_id, g.lid, g.cid, r.nodes AS renamed,
       r.nodes -> ('$.' || g.cid || '.inputs.color') AS col,
       r.nodes -> ('$.' || g.cid || '.inputs.brightness') AS br,
       r.nodes -> ('$.' || g.cid || '.inputs.alpha') AS al
FROM graphs g JOIN renamed r ON r.color_id = g.color_id;

-- A flat colour times its brightness and alpha.
UPDATE clips
SET graph_json = json_set(clips.graph_json, '$.nodes.' || f.lid || '.inputs.color', json_array(
    min(1, max(0, (coalesce(f.col, '[1, 1, 1]') ->> '$[0]') * f.level)),
    min(1, max(0, (coalesce(f.col, '[1, 1, 1]') ->> '$[1]') * f.level)),
    min(1, max(0, (coalesce(f.col, '[1, 1, 1]') ->> '$[2]') * f.level))))
FROM (
    SELECT laser_id, lid, col, coalesce(br ->> '$', 1) * coalesce(al ->> '$', 1) AS level
    FROM laser_fold
    WHERE coalesce(json_type(col), 'array') = 'array'
      AND coalesce(json_type(br), 'real') IN ('integer', 'real')
      AND coalesce(json_type(al), 'real') IN ('integer', 'real')
) f
WHERE clips.id = f.laser_id;

-- Anything wired: the Color graph's nodes, and color the product of its
-- colour (a one-stop gradient when flat), brightness and alpha.
CREATE TEMP TABLE laser_fold_wired AS
SELECT f.laser_id, f.lid,
       json_patch(
           (SELECT json_group_object('c_' || n.key, json(n.value)) FROM json_each(f.renamed) n WHERE n.key <> f.cid),
           CASE WHEN json_type(f.col) = 'object' THEN '{}' ELSE json_object(
               'c_once', json_object('kind', 'time'),
               'c_flat', json_object(
                   'kind', 'curve',
                   'settings', json_object('kind', 'color'),
                   'inputs', json_object(
                       'x', json_object('node', 'c_once'),
                       'gradient', json_object('stops', json_array(json_object(
                           't', 0, 'color', json(coalesce(f.col, '[1, 1, 1]')))))))) END
       ) AS nodes,
       (SELECT json_group_array(json(item.v)) FROM (
           SELECT 1 AS o, CASE WHEN json_type(f.col) = 'object' THEN f.col ELSE '{"node":"c_flat"}' END AS v
           UNION ALL SELECT 2, f.br
           UNION ALL SELECT 3, f.al
           ORDER BY o
       ) item WHERE item.v IS NOT NULL) AS items
FROM laser_fold f
WHERE NOT (coalesce(json_type(f.col), 'array') = 'array'
      AND coalesce(json_type(f.br), 'real') IN ('integer', 'real')
      AND coalesce(json_type(f.al), 'real') IN ('integer', 'real'));

UPDATE clips
SET graph_json = json_set(
    json_patch(json(clips.graph_json), json_object('nodes', json(json_patch(f.nodes,
        CASE WHEN json_array_length(f.items) > 1 THEN json_object('c_mix', json_object(
            'kind', 'math',
            'settings', json_object('op', '*'),
            'inputs', json_object('values', json(f.items)))) ELSE '{}' END)))),
    '$.nodes.' || f.lid || '.inputs.color',
    json(CASE WHEN json_array_length(f.items) > 1 THEN json_object('node', 'c_mix') ELSE f.items -> '$[0]' END))
FROM laser_fold_wired f
WHERE clips.id = f.laser_id;

-- A folded Color clip over one group of only projectors lit nothing else.
DELETE FROM clips WHERE id IN (
    SELECT p.color_id
    FROM laser_color_pairs p
    JOIN clips c ON c.id = p.color_id
    JOIN scores s ON s.id = c.score_id
    JOIN fixture_groups g ON g.venue_id = s.venue_id AND g.name = c.selection_json ->> '$.expression'
    WHERE EXISTS (SELECT 1 FROM fixture_group_members m WHERE m.group_id = g.id)
      AND NOT EXISTS (
          SELECT 1 FROM fixture_group_members m
          JOIN fixtures f ON f.id = m.fixture_id
          WHERE m.group_id = g.id AND f.fixture_path <> 'Generic/Generic-ILDA-Laser-Projector.qxf')
);

DELETE FROM drafts WHERE base_json LIKE '%"laser"%' OR state_json LIKE '%"laser"%';

DROP TABLE laser_old;
DROP TABLE laser_read;
DROP TABLE laser_spread;
DROP TABLE laser_added;
DROP TABLE laser_color_pairs;
DROP TABLE laser_fold;
DROP TABLE laser_fold_wired;
