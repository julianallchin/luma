Migrations are versioned SQL files that run in order (by timestamped filename) when `sqlx::migrate!` executes at startup.

- Keep existing files unchanged; add a new `YYYYMMDDHHMMSS_description.sql` for each schema change.
- On fresh installs, all files run; on existing installs, only new files run because applied versions are tracked in `_sqlx_migrations` (automatically).
- If you need to backfill data or add columns, put the exact SQL in a new migration file.
