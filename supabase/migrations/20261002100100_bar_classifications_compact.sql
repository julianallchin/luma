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
-- The column stays text. Rows already in the new shape (an object, not an
-- array) are left alone, so the file can run twice. Each converted row
-- changes, so PowerSync sends it to every device again.
--
-- Symmetric with backend/migrations/20261002100100_bar_classifications_compact.sql.
--
-- Order:
--   1. Apply this file.
--   2. Ship the app build that writes and reads the compact shape. An older
--      build cannot read a converted row, and a row it uploads is in the old
--      shape, which the new build cannot read either: run this file again
--      once no older build is left.

begin;

-- jsonb prints ", " and ": "; the replace drops those spaces, which is safe
-- because the only strings in the object are its three keys.
update public.track_bar_classifications cls
set classifications_json = replace(jsonb_build_object(
        'first_bar', jsonb_build_array(old.bars -> 0 -> 'start', old.bars -> 0 -> 'end'),
        'intensity', coalesce((
            select jsonb_agg(
                       trim_scale(round((bar.value -> 'predictions' ->> 'intensity')::numeric, 3))
                       order by n)
              from generate_series(0, old.last_idx) n
              left join jsonb_array_elements(old.bars) bar
                on (bar.value ->> 'bar_idx')::int = n), '[]'::jsonb),
        'scores', coalesce((
            select jsonb_agg(
                       case when bar.value is null then 'null'::jsonb else (
                           select jsonb_agg(
                                      trim_scale(round((bar.value -> 'predictions' ->> tag.name)::numeric, 3))
                                      order by tag.ord)
                             from jsonb_array_elements_text(cls.tag_order_json::jsonb)
                                  with ordinality tag (name, ord))
                       end
                       order by n)
              from generate_series(0, old.last_idx) n
              left join jsonb_array_elements(old.bars) bar
                on (bar.value ->> 'bar_idx')::int = n), '[]'::jsonb)
    )::text, ' ', ''),
    updated_at = now()
from (
    select id,
           classifications_json::jsonb as bars,
           (select max((b ->> 'bar_idx')::int)
              from jsonb_array_elements(classifications_json::jsonb) b) as last_idx
      from public.track_bar_classifications
     where jsonb_typeof(classifications_json::jsonb) = 'array'
) old
where old.id = cls.id;

commit;
