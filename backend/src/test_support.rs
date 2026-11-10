//! A library for a test to run commands against.

use std::path::{Path, PathBuf};

use crate::boot::{Boot, Identity};
use crate::database::local::{auth, state};
use crate::dispatch::AppServices;
use crate::events::Events;
use crate::storage::StorageRoot;

/// The principal a signed-in test library belongs to. Every synced row has an
/// owner, and rows a test expects to see must carry this one.
pub(crate) const TEST_OWNER: &str = "11111111-2222-3333-4444-555555555555";

/// A library in `directory`, signed in as [`TEST_OWNER`], booted the way a
/// windowless host boots one.
pub(crate) async fn signed_in(directory: &Path, fixtures_root: PathBuf) -> AppServices {
    let state_db = state::init_state_db_at(directory).await.unwrap();
    auth::install_test_session(&state_db.0, TEST_OWNER).await;
    state_db.0.close().await;
    open(directory, fixtures_root).await
}

/// A library in `directory` nobody has signed in to, as a windowless host
/// finds one before the app has: its commands refuse.
pub(crate) async fn signed_out(directory: &Path, fixtures_root: PathBuf) -> AppServices {
    open(directory, fixtures_root).await
}

async fn open(directory: &Path, fixtures_root: PathBuf) -> AppServices {
    crate::boot::open(Boot {
        storage: StorageRoot::from_path(directory.to_path_buf()),
        fixtures_root,
        cache_dir: None,
        events: Events::discard(),
        identity: Identity::Stored,
        sync: false,
        output: false,
        audio: false,
    })
    .await
    .unwrap()
    .services
}

/// The repository root: this crate sits at `<repo>/backend`.
pub(crate) fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate has a parent directory")
        .to_path_buf()
}

/// `value` as a JSON golden's text: keys sorted, because this crate's
/// serde_json keeps insertion order and a golden's bytes must not depend on it.
pub(crate) fn golden_text(mut value: serde_json::Value) -> String {
    value.sort_all_objects();
    let mut out = serde_json::to_string_pretty(&value).expect("the capture serializes");
    out.push('\n');
    out
}
