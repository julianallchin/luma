-- Remote counterpart of `fixtures.address_pinned`
-- (`src-tauri/migrations/20260830000000_patch_addressing.sql`, part 1).
--
-- Deployed 2026-08-30. The column is absent from the `fixtures` entry in
-- `src-tauri/src/sync/registry.rs`, so nothing pushes it and nothing pulls it.
-- The filename says so on purpose: rename it to
-- `20260831000000_fixtures_address_pinned.sql` only when it is actually
-- applied, and only together with step 2 below.
--
-- Why it has to exist: `address` and `universe` sync, `address_pinned` does
-- not, so the pin can be *relocated* by somebody else's push. A pins fixture F
-- at 1/100; B auto-patches F to 3/17 and pushes; A pulls and now holds a pin
-- at 3/17 — a hand-chosen address nobody chose, and one auto-patch will never
-- move again. The pin is a property of the address, so it must travel with it.
--
-- To land it (in this order — the app must never register a column the remote
-- does not have):
--   1. Deploy this migration.
--   2. Add "address_pinned" to the `fixtures` column list in
--      `src-tauri/src/sync/registry.rs`. It is the only change: `fixtures`
--      already uses the generic column-driven push/pull, so the value rides
--      along once it is listed.
--   3. Ship, then delete the "until the pin syncs" note in
--      `src-tauri/migrations/20260831000000_patch_width_repair.sql` part 2 and
--      turn `a_pulled_address_keeps_a_local_pin_until_the_pin_syncs` in
--      `src-tauri/src/sync/tests.rs` around: the pin should follow the pusher,
--      not the puller.
--
-- Older clients keep sending rows without the key; the default covers them,
-- and an unpinned fixture is the safe reading — auto-patch may move it.
--
-- `integer`, not `boolean`, because the push side is untyped: `sync::
-- orchestrator::read_record_as_json` turns a SQLite INTEGER into a JSON
-- number, and PostgREST refuses `0` for a `boolean` column. The CHECK is what
-- keeps it a flag.

ALTER TABLE public.fixtures
    ADD COLUMN address_pinned integer NOT NULL DEFAULT 0
    CHECK (address_pinned IN (0, 1));
