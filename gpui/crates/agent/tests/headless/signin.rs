//! Launching with every shape of stored session, through the production app
//! tree.
//!
//! The bug this pins: a spent Supabase refresh token used to abort `main`
//! before a window existed (`could not open the library: … refresh_token_
//! already_used`). A session that proves a principal offline opens the app
//! without asking the network anything; one that does not is asked online,
//! behind a splash, and lands at the gate when the answer is no. Nothing
//! opens without a principal — there is no guest door.

#![cfg(feature = "app")]

use super::support;
use support::session::{self, Stored};

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AnyView, App, AppContext as _, Window};
use gpui_agent::{Config, Harness, Mode, GPU_LIVENESS_TIMEOUT};
use serde_json::Value;

async fn seed(dir: &Path, stored: Stored) {
    let db = luma_lib::database::local::database::init_app_db_at(dir)
        .await
        .expect("failed to open fixture app database");
    // Owned by whoever this launch will admit: `auth_visible_venues` shows the
    // admitted principal's rows, so seeing this venue is itself proof that
    // admission was armed for the right identity.
    //
    sqlx::query("INSERT INTO venues (id, uid, name) VALUES ('venue', ?, 'Sign-in Venue')")
    .bind(session::owner(stored))
    .execute(&db.0)
    .await
    .expect("failed to seed venue");
    db.0.close().await;
    session::seed(dir, stored).await;
}

fn fixture_dir(name: &str, stored: Stored) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luma-gpui-signin-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("failed to create fixture directory");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to start fixture runtime")
        .block_on(seed(&dir, stored));
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
fn a_failed_session_refresh_offers_retry_and_a_sign_in_route() {
    let dir = fixture_dir("unproven", Stored::Unproven);
    let mut harness = harness(&dir);
    let out = exec(
        &mut harness,
        r#"
        const failure = until("the refresh failure", s =>
            s.find({ role: "button", label: "Retry" }) !== undefined);
        if (failure.find(n => n.role === "text" && n.label.startsWith("Your session expired"))) {
            throw new Error("A refresh error was mislabeled as expiry");
        }
        app.click(failure.find({ role: "button", label: "Sign in again" }));
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
            // …but it says why the person is here.
            expired: gate.find((n) => n.role === "text"
                && n.label.startsWith("Your session expired")) !== undefined,
            // Escape is not a door. The gate is the app until someone signs in.
            held: (() => {
                app.key("escape");
                return app.snapshot().find({ role: "text", label: "Sign in to Luma" }) !== undefined;
            })(),
        })
    "#,
    );
    assert_eq!(out["field"], true, "the gate shows an email field");
    assert_eq!(out["submit"], true, "the gate offers its primary capsule");
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
    assert_eq!(out["offline"], false, "there is no guest door");
    assert_eq!(
        out["expired"], false,
        "a refresh error does not prove expiry"
    );
    assert_eq!(out["held"], true, "escape must not get past the gate");
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
/// presses — and land at the gate: signed out is not a state the shell shows.
///
/// The principal's one row is already durable, so the cloud flush sign-out
/// begins with has nothing to send and the whole boundary runs offline. That
/// is the point: what is under test is the *gesture*, from the foot to the end
/// state, not the sync it fronts.
#[test]
fn signing_out_from_the_account_foot_lands_at_the_gate() {
    let dir = fixture_dir("signout", Stored::Proven { expires_in: 3600 });
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
        // Either the gesture lands — the gate is the next screen — or it
        // failed, and the failure has to be readable from right here, because
        // the person who pressed this never opened settings.
        const settled = until("sign-out to settle", (s) =>
            s.find({ role: "text", label: "Sign in to Luma" }) !== undefined
                || s.find((n) => n.role === "text"
                    && n.label.startsWith("Could not sign out")) !== undefined
                ? s : undefined);
        ({
            before,
            gate: settled.find({ role: "text", label: "Sign in to Luma" }) !== undefined,
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
    assert_eq!(out["gate"], true, "signing out must land at the gate");
    assert_eq!(out["menu"], false, "the account menu stayed open");
}

