-- Swap pos_y and pos_z to convert from Y-up (Unity/Three.js) to Z-up (lighting convention)
-- Z-up convention: pos_x = left/right, pos_y = front/back (depth), pos_z = up/down (height)

-- Add temporary columns for the swap
ALTER TABLE fixtures ADD COLUMN temp_pos_y REAL;
ALTER TABLE fixtures ADD COLUMN temp_pos_z REAL;
ALTER TABLE fixtures ADD COLUMN temp_rot_y REAL;
ALTER TABLE fixtures ADD COLUMN temp_rot_z REAL;

-- Copy current values to temp
UPDATE fixtures SET
    temp_pos_y = pos_y,
    temp_pos_z = pos_z,
    temp_rot_y = rot_y,
    temp_rot_z = rot_z;

-- Swap positions: old Y (height) becomes new Z, old Z (depth) becomes new Y
UPDATE fixtures SET
    pos_y = temp_pos_z,
    pos_z = temp_pos_y,
    rot_y = temp_rot_z,
    rot_z = temp_rot_y;

-- Drop temporary columns
ALTER TABLE fixtures DROP COLUMN temp_pos_y;
ALTER TABLE fixtures DROP COLUMN temp_pos_z;
ALTER TABLE fixtures DROP COLUMN temp_rot_y;
ALTER TABLE fixtures DROP COLUMN temp_rot_z;
