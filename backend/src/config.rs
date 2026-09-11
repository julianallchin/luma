pub const SUPABASE_URL: &str = "https://smuuycypmsutwrkpctws.supabase.co";
pub const SUPABASE_ANON_KEY: &str = "sb_publishable_V8JRQkGliRYDAiGghjUrmQ_w8fpfjRb";

/// The PowerSync Cloud instance URL, e.g. `https://<id>.powersync.journeyapps.com`.
///
/// The operator fills this in after creating the instance and pointing it at
/// the Supabase Postgres — see the "PowerSync Cloud" section of
/// `docs/design/sync.md`. Empty means cloud sync stays off.
pub const POWERSYNC_URL: &str = "https://6aa483d7a77ca1231d276892.powersync.journeyapps.com";

/// The PowerSync endpoint this build connects to.
#[must_use]
pub fn powersync_url() -> String {
    std::env::var("LUMA_POWERSYNC_URL").unwrap_or_else(|_| POWERSYNC_URL.to_owned())
}

/// Where uploads go. Supabase exposes PostgREST under `/rest/v1`, so this is
/// derived rather than configured — a second constant could only ever disagree
/// with `SUPABASE_URL`.
#[must_use]
pub fn postgrest_url() -> String {
    std::env::var("LUMA_POSTGREST_URL")
        .unwrap_or_else(|_| format!("{}/rest/v1", SUPABASE_URL.trim_end_matches('/')))
}

/// The anon key PostgREST wants in `apikey`.
#[must_use]
pub fn supabase_anon_key() -> String {
    std::env::var("LUMA_SUPABASE_ANON_KEY").unwrap_or_else(|_| SUPABASE_ANON_KEY.to_owned())
}
