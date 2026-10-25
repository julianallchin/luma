Migrations are versioned SQL files that run in order (by timestamped filename) when `sqlx::migrate!` executes at startup.

- Keep existing files unchanged; add a new `YYYYMMDDHHMMSS_description.sql` for each schema change.
- On fresh installs, all files run; on existing installs, only new files run because applied versions are tracked in `_sqlx_migrations` (automatically).
- A local migration is DDL only. The synced tables are a projection of the server: a data or format change is a server migration in `supabase/migrations`, never a rewrite of local rows. When a client can no longer read what an older one downloaded, bump `PROJECTION_VERSION` in `src/database/local/database.rs`; a mismatch at startup clears the synced tables, without uploading anything, and PowerSync downloads them again.
