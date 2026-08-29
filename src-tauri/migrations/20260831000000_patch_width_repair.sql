-- The other half of the footprint repair, and what the pin still costs.
--
-- Amends `20260830000000_patch_addressing.sql`, which is applied and therefore
-- frozen (sqlx checksums it).

-- 1. A width wider than a universe was left behind by the first repair.
--
-- That pass clamped `num_channels` *up* to 1 and moved a bad `address` to 1,
-- but never clamped a width *down*. The triggers it installed fire on
-- `address + num_channels - 1 > 512`, which no address can satisfy once the
-- width alone exceeds 512 — so a row with, say, 600 channels survived the
-- repair and then became permanently un-updatable: every UPDATE against it
-- aborts, and `auto_patch` writes the whole venue in one transaction, so one
-- such row took the venue's addressing with it.
--
-- 512 is the clamp because it is the widest footprint that exists; the row was
-- never playable as written, and auto-patch is what puts it somewhere real.
UPDATE fixtures SET num_channels = 512 WHERE num_channels > 512;
UPDATE fixtures SET address = 1 WHERE address < 1 OR address + num_channels - 1 > 512;

-- No new constraint goes with this: `num_channels BETWEEN 1 AND 512` is
-- already total under the two `fixtures_address_fits_universe_*` triggers,
-- because `address >= 1` is one of their clauses and `address + n - 1 > 512`
-- is another. A separate CHECK would be a second way to say the same thing —
-- and adding one to an existing table means rebuilding it.
--
-- `sync::pull::repair_incoming` clamps both halves the same way at the pull
-- boundary, for the same reason it repairs the address: the remote table has
-- neither trigger, and an aborted upsert wedges the table's cursor forever.

-- 2. `address_pinned` does not sync yet, and the pin can be relocated.
--
-- The column is local (`20260830000000`, part 1) while `address` and
-- `universe` are pulled from the remote. So: machine A pins fixture F at
-- 1/100; machine B auto-patches F to 3/17 and pushes; A pulls, and F is now
-- *pinned at 3/17* — a hand-chosen address nobody chose. The pin travels with
-- the address, which is the correct end state and needs a remote column;
-- `supabase/migrations/20260831000000_fixtures_address_pinned_NOT_DEPLOYED.sql`
-- is that column, with the checklist for landing it. Until it is deployed the
-- behaviour above stands, and `sync::tests` pins it so the fix is a test
-- change rather than a discovery.
--
-- Not fixable locally in the meantime: clearing the pin on a pulled address
-- would silently unpin fixtures whenever anyone else touched the venue, which
-- is the louder failure of the two.
