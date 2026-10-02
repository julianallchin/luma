-- Bar classifications become compact. `classifications_json` was one object
-- per scored bar, each carrying its start, end and 23 named 17-digit floats.
-- It is now
--
--   {"first_bar": [start, end], "intensity": [i0, ...], "scores": [[s0, ...], ...]}
--
-- Index i is bar i of the beat grid, so a bar's start and end come from the
-- grid. `scores` follows the row's `tag_order_json`. A bar the worker skipped
-- is null. Every value is rounded to 3 decimals. `first_bar` is the old first
-- entry's span, which is what the staleness check compares to the grid now.
--
-- Rows already in the new shape (an object, not an array) are left alone.
-- A migration connection carries no upload triggers, so nothing is queued for
-- upload: the server converts its own rows with the symmetric file, and sync
-- brings those down.
--
-- Symmetric with supabase/migrations/20261002100100_bar_classifications_compact.sql.

CREATE TEMP TABLE old_bars AS
SELECT cls.track_id,
       json_extract(bar.value, '$.bar_idx') AS idx,
       round(json_extract(bar.value, '$.predictions.intensity'), 3) AS intensity,
       (SELECT json_group_array(
                   round(json_extract(bar.value, '$.predictions.' || tag.value), 3)
                   ORDER BY tag.key)
          FROM json_each(cls.tag_order_json) tag) AS scores
FROM track_bar_classifications cls, json_each(cls.classifications_json) bar
WHERE json_type(cls.classifications_json) = 'array';

CREATE INDEX temp.old_bars_idx ON old_bars (track_id, idx);

-- 0 .. the highest bar index any row has.
CREATE TEMP TABLE bar_numbers AS
WITH RECURSIVE n(i) AS (
    SELECT 0
    UNION ALL
    SELECT i + 1 FROM n WHERE i < (SELECT max(idx) FROM old_bars)
)
SELECT i FROM n;

UPDATE track_bar_classifications
SET classifications_json = json_object(
    'first_bar', json_array(
        json_extract(classifications_json, '$[0].start'),
        json_extract(classifications_json, '$[0].end')),
    'intensity', (
        SELECT json_group_array(bar.intensity ORDER BY n.i)
        FROM bar_numbers n
        LEFT JOIN old_bars bar ON bar.track_id = track_bar_classifications.track_id AND bar.idx = n.i
        WHERE n.i <= (SELECT max(idx) FROM old_bars
                      WHERE track_id = track_bar_classifications.track_id)),
    'scores', (
        SELECT json_group_array(json(bar.scores) ORDER BY n.i)
        FROM bar_numbers n
        LEFT JOIN old_bars bar ON bar.track_id = track_bar_classifications.track_id AND bar.idx = n.i
        WHERE n.i <= (SELECT max(idx) FROM old_bars
                      WHERE track_id = track_bar_classifications.track_id)))
WHERE json_type(classifications_json) = 'array';

DROP TABLE old_bars;
DROP TABLE bar_numbers;
