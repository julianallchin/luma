//! Launching with every shape of stored session, through the production app
//! tree.
//!
//! The bug this pins: a spent Supabase refresh token used to abort `main`
//! before a window existed (`could not open the library: … refresh_token_
//! already_used`). Boot no longer asks the network anything, so the token's
//! liveness cannot decide whether Luma opens — only whether the *cloud* works,
//! which is a later, recoverable question. What decides the first screen is
//! whether the stored session still proves a principal offline.

#![cfg(feature = "app")]

use super::support;
use support::session::{self, Stored};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;

async fn seed(dir: &Path, stored: Stored, synced: bool) {
    let db = luma_lib::database::local::database::init_app_db_at(dir)
        .await
        .expect("failed to open fixture app database");
    // Owned by whoever this launch will admit: `auth_visible_venues` shows the
    // admitted principal's rows, so seeing this venue is itself proof that
    // admission was armed for the right identity.
    //
    // `synced` decides whether it has reached the cloud. Sign-out flushes the
    // principal's un-synced rows before it deletes anything, so a row that is
    // already durable is the only way to reach the far side of that flush
    // without a network — and a row that is not is how the refusal is
    // provoked on purpose.
    sqlx::query(
        "INSERT INTO venues (id, uid, name, synced_at) VALUES ('venue', ?, 'Sign-in Venue', ?)",
    )
    .bind(session::owner(stored))
    .bind(synced.then_some("2026-01-01T00:00:00Z"))
    .execute(&db.0)
    .await
    .expect("failed to seed venue");
    db.0.close().await;
    session::seed(dir, stored).await;
}

fn fixture_dir(name: &str, stored: Stored) -> PathBuf {
    fixture_dir_at(name, stored, false)
}

/// [`fixture_dir`] saying whether the seeded venue has already reached the
/// cloud — see [`seed`].
fn fixture_dir_at(name: &str, stored: Stored, synced: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-signin-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create fixture directory");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start fixture runtime")
        .block_on(seed(&dir, stored, synced));
    dir
}

fn harness(dir: &Path) -> Harness {
    let root: gpui_agent::RootFactory =
        Arc::new(move |window: &mut Window, cx: &mut App| -> AnyView {
            luma_app::init(cx);
            // The assertion under test: this is the call that used to return
            // `Err` and take the process with it.
            let library = luma_app::Library::open().expect("failed to open fixture library");
            let luma = cx.new(|cx| luma_app::Luma::new(library, cx));
            cx.new(|cx| gpui_component::Root::new(luma, window, cx).bordered(false))
                .into()
        });
    Harness::headless(
        Config {
            mode: Mode::Headless,
            call_timeout: GPU_LIVENESS_TIMEOUT,
            runtime: support::runtime(dir),
            ..Config::default()
        },
        root,
    )
    .expect("failed to start harness")
}

fn exec(harness: &mut Harness, script: &str) -> Value {
    let result = harness.exec(&support::script(script), Duration::from_secs(90));
    assert_eq!(result.error, None, "script failed:\n{}", result.stdout);
    result.result
}

#[test]
fn a_session_that_proves_nobody_launches_signed_out_at_the_gate() {
    let dir = fixture_dir("unproven", Stored::Unproven);
    let mut harness = harness(&dir);
    let out = exec(
        &mut harness,
        r#"
        const gate = until("the sign-in screen", (s) =>
            s.find({ role: "text", label: "Sign in to Luma" }) !== undefined);
        ({
            field: gate.find({ role: "input", label: "Email" }) !== undefined,
            submit: gate.find({ role: "button", label: "Continue" }) !== undefined,
            offline: gate.find({ role: "button", label: "Work offline" }) !== undefined,
            venues: gate.find({ role: "card", label: "Venue dialog" }) !== undefined,
            // A state, not a plane: the shell is not behind it, so neither
            // pane toggle is there to be pressed through it.
            shell: gate.find({ role: "button", label: "sidebar-toggle" }) !== undefined
                || gate.find({ role: "button", label: "panel-toggle" }) !== undefined,
            // …and the window still moves and closes.
            lights: gate.find({ role: "button", label: "close" }) !== undefined,
            // Nothing failed. A launch that could not verify a session is a
            // signed-out launch, not an error to report.
            failure: gate.find((n) => n.role === "text"
                && n.label.toLowerCase().includes("could not open")) !== undefined,
            // Escape is the offline door, and it opens onto the venue picker
            // the launch was heading for.
            dismissed: (() => {
                app.key("escape");
                return until("the shell the gate stood in front of", (s) =>
                    s.find({ role: "card", label: "Venue dialog" }) !== undefined
                        && s.find({ role: "text", label: "Sign in to Luma" }) === undefined)
                    !== undefined;
            })(),
        })
    "#,
    );
    assert_eq!(out["field"], true, "the gate shows an email field");
    assert_eq!(out["submit"], true, "the gate offers its primary capsule");
    assert_eq!(out["offline"], true, "the gate offers a way past it");
    assert_eq!(out["venues"], false, "the gate replaces the venue picker");
    assert_eq!(
        out["shell"], false,
        "the gate replaces the shell, it does not cover it"
    );
    assert_eq!(
        out["lights"], true,
        "the screen carries the window's own controls"
    );
    assert_eq!(out["failure"], false, "nothing is reported as a failure");
    assert_eq!(
        out["dismissed"], true,
        "escape works offline, it does not trap"
    );

    // The gate is not the only way back to it: settings is where an account
    // lives here, and a guest must be able to sign in from there later.
    let account = exec(
        &mut harness,
        r#"
        // The venue picker will not stand aside until a room is chosen, and
        // a guest's own rows are exactly what is still readable here.
        nav.venue("Sign-in Venue");
        until("the shell", (s) =>
            s.find({ role: "button", label: "Account" }) !== undefined
                && s.find({ role: "card", label: "Venue dialog" }) === undefined);
        // The foot names the guest namespace before anything is opened.
        const foot = app.snapshot().find({ role: "text", label: "Working locally" }) !== undefined;
        nav.settings();
        const settings = until("the settings dialog", (s) =>
            s.find({ role: "toggle", label: "Account" }) !== undefined);
        app.click(settings.find({ role: "toggle", label: "Account" }));
        const shown = until("the account section", (s) =>
            s.find({ role: "button", label: "Sign in" }) !== undefined);
        app.click(shown.find({ role: "button", label: "Sign in" }));
        const reopened = until("the gate reopened from settings", (s) =>
            s.find({ role: "text", label: "Sign in to Luma" }) !== undefined
                // Settings went with the shell rather than lingering under it.
                && s.find({ role: "toggle", label: "Account" }) === undefined);
        ({
            foot,
            identity: shown.find({ role: "input", label: "Working locally" })
                !== undefined,
            reopened: reopened.find({ role: "button", label: "Continue" }) !== undefined,
        })
    "#,
    );
    assert_eq!(
        account["foot"], true,
        "the sidebar's foot names the guest namespace"
    );
    assert_eq!(
        account["identity"], true,
        "the account section names the guest namespace"
    );
    assert_eq!(
        account["reopened"], true,
        "settings can reach the gate after it was dismissed"
    );
}

#[test]
fn a_proven_session_launches_past_the_gate_even_with_a_dead_refresh_token() {
    // Expired access token, spent refresh token: the exact state that used to
    // abort `main`. Boot proves the principal from the stored proof and never
    // reaches for the token, so the app opens where a signed-in app opens.
    let dir = fixture_dir("proven", Stored::Proven { expires_in: -3600 });
    let mut harness = harness(&dir);
    let out = exec(
        &mut harness,
        r#"
        const shown = until("the venue picker", (s) =>
            s.find({ role: "card", label: "Sign-in Venue" }) !== undefined);
        ({ gate: shown.find({ role: "text", label: "Sign in to Luma" }) !== undefined })
    "#,
    );
    assert_eq!(out["gate"], false, "a proven session skips the gate");
}

/// Sign out from the sidebar foot's account menu — the door a person actually
/// presses — and land in the guest namespace.
///
/// The principal's one row is already durable, so the cloud flush sign-out
/// begins with has nothing to send and the whole boundary runs offline. That
/// is the point: what is under test is the *gesture*, from the foot to the end
/// state, not the sync it fronts.
#[test]
fn signing_out_from_the_account_foot_reaches_the_guest_namespace() {
    let dir = fixture_dir_at("signout", Stored::Proven { expires_in: 3600 }, true);
    let mut harness = harness(&dir);
    let out = exec(
        &mut harness,
        r#"
        nav.venue("Sign-in Venue");
        until("the shell", (s) =>
            s.find({ role: "button", label: "Account" }) !== undefined
                && s.find({ role: "card", label: "Venue dialog" }) === undefined);
        // Signed in, so the foot does not name the guest namespace yet.
        const before = app.snapshot().find({ role: "text", label: "Working locally" }) === undefined;
        nav.step("the account foot", "button", "Account");
        nav.step("the sign-out row", "row", "Sign out");
        // Either the gesture lands — the foot names the guest namespace — or
        // it failed, and the failure has to be readable from right here,
        // because the person who pressed this never opened settings.
        const settled = until("sign-out to settle", (s) =>
            s.find({ role: "text", label: "Working locally" }) !== undefined
                || s.find((n) => n.role === "text"
                    && n.label.startsWith("Could not sign out")) !== undefined
                ? s : undefined);
        ({
            before,
            guest: settled.find({ role: "text", label: "Working locally" }) !== undefined,
            failure: (settled.find((n) => n.role === "text"
                && n.label.startsWith("Could not sign out")) || {}).label || null,
            // The menu went with the gesture rather than sitting open over it.
            menu: settled.find({ role: "row", label: "Sign out" }) !== undefined,
        })
    "#,
    );
    assert_eq!(out["before"], true, "the fixture launched signed in");
    assert_eq!(
        out["failure"],
        Value::Null,
        "sign-out reported a failure: {}",
        out["failure"]
    );
    assert_eq!(
        out["guest"], true,
        "the foot does not name the guest namespace after signing out"
    );
    assert_eq!(out["menu"], false, "the account menu stayed open");
}

/// A sign-out that cannot land says so at the foot it was pressed from.
///
/// The principal owns a venue that has never reached the cloud, and the stored
/// token cannot flush it — so the host refuses, which is the whole contract of
/// `wipe_database`: offline is never permission to discard the only copy. What
/// this pins is the *report*. The failure used to land in a field only the
/// settings screen renders, so pressing sign-out from the sidebar looked like
/// pressing nothing at all.
#[test]
fn a_sign_out_that_cannot_flush_says_so_at_the_foot() {
    let dir = fixture_dir("signout-offline", Stored::Proven { expires_in: 3600 });
    let mut harness = harness(&dir);
    let out = exec(
        &mut harness,
        r#"
        nav.venue("Sign-in Venue");
        until("the shell", (s) =>
            s.find({ role: "button", label: "Account" }) !== undefined
                && s.find({ role: "card", label: "Venue dialog" }) === undefined);
        nav.step("the account foot", "button", "Account");
        nav.step("the sign-out row", "row", "Sign out");
        const settled = until("the refusal", (s) =>
            s.find((n) => n.role === "text"
                && n.label.startsWith("Could not sign out")) !== undefined
                || s.find({ role: "text", label: "Working locally" }) !== undefined
                ? s : undefined);
        ({
            failure: (settled.find((n) => n.role === "text"
                && n.label.startsWith("Could not sign out")) || {}).label || null,
            // Nothing half-happened: the account is still this library's.
            guest: settled.find({ role: "text", label: "Working locally" }) !== undefined,
            // …and the gesture is pressable again rather than stuck mid-flight.
            retry: (() => {
                app.click(app.snapshot().find({ role: "button", label: "Account" }));
                return until("the menu, offering the gesture again", (s) => {
                    const row = s.find({ role: "row", label: "Sign out" });
                    return row !== undefined && row.enabled !== false ? s : undefined;
                }) !== undefined;
            })(),
        })
    "#,
    );
    assert!(
        out["failure"]
            .as_str()
            .is_some_and(|text| text.contains("Cannot sign out before catalog sync")),
        "the refusal is not reported at the foot: {}",
        out["failure"]
    );
    assert_eq!(
        out["guest"], false,
        "a refused sign-out must not read as signed out"
    );
    assert_eq!(out["retry"], true, "the gesture is stuck after a failure");
}
