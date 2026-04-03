-- Replace 1D display_order with 2D display_x / display_y for free-form grid placement.
-- Backfill: lay existing cues out in a 4-column grid using their current display_order.
ALTER TABLE cues ADD COLUMN display_x INTEGER NOT NULL DEFAULT 0;
ALTER TABLE cues ADD COLUMN display_y INTEGER NOT NULL DEFAULT 0;

UPDATE cues SET
  display_x = (display_order % 4),
  display_y = (display_order / 4);
