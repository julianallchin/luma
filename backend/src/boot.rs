//! Opening a library: the one startup order every host follows.
//!
//! The desktop app, the `luma-cli` hosts, the GPUI harness and the backend's
//! own tests all reach a library through [`open`]. What differs between them
//! is said in [`Boot`] — whose session to trust, whether to replicate, which
//! devices to touch — and never by a host repeating the steps in its own order.

use std::path::PathBuf;
use std::sync::Arc;

use crate::agent_execution::{headless_env, workspace::PythonWorkspaceService};
use crate::database::local::auth;
use crate::database::local::database::{init_app_db_at, open_app_db_at};
use crate::database::local::state::init_state_db_at;
use crate::dispatch::AppServices;
use crate::events::Events;
use crate::storage::StorageRoot;

/// Whose library this process opens.
pub enum Identity {
    /// Trust this principal without a verified session. Only for a disposable
    /// library the caller created: it arms the write gate for an id nobody
    /// proved.
    Fixture(String),
    /// The session the app stored on this machine. A session that no longer
    /// proves anyone refuses the boot: a process without a sign-in screen
    /// cannot repair it, and must not guess. With no session at all the
    /// library opens signed out, and its commands refuse until the app signs
    /// someone in.
    Stored,
    /// The stored session, or signed out when it proves nobody. The app,
    /// whose sign-in screen is the way out of that.
    StoredOrSignedOut,
}

/// What a host asks of [`open`].
pub struct Boot {
    pub storage: StorageRoot,
    pub fixtures_root: PathBuf,
    /// Where the managed Python environment lives. `None` is a host that runs
    /// no Python cell; a cell there fails with that reason.
    pub cache_dir: Option<PathBuf>,
    pub events: Events,
    pub identity: Identity,
    /// Replicate records through PowerSync. A replica that cannot start is
    /// reported, not a boot failure.
    pub sync: bool,
    /// DMX output and MIDI/OSC inputs: bind sockets and open devices.
    pub output: bool,
    /// Let the persisted audio setting open the machine's audio device.
    pub audio: bool,
}

/// An opened library.
pub struct Booted {
    pub services: AppServices,
    /// Whom the library was opened for; `None` is nobody signed in.
    pub principal: Option<String>,
    /// Why a stored session proved nobody, under
    /// [`Identity::StoredOrSignedOut`].
    pub lapsed: Option<String>,
}

/// Open the library: migrate both databases, arm write admission, assemble
/// the services, start replication and devices as asked, apply the persisted
/// settings, and recover the agent rows a quit left in progress.
///
/// Boot never touches the network for identity: the principal comes from the
/// session the app already verified (see [`auth::bootstrap_headless_admission`]
/// for why refreshing it here would sign the app out) or from
/// [`Identity::Fixture`].
///
/// # Errors
///
/// If a migration fails, write admission cannot be armed, a stored session
/// proves nobody under [`Identity::Stored`], output cannot start, or the
/// persisted settings cannot be applied.
pub async fn open(boot: Boot) -> Result<Booted, String> {
    let Boot {
        storage,
        fixtures_root,
        cache_dir,
        events,
        identity,
        sync,
        output,
        audio,
    } = boot;

    // Every host finds the bundled ffmpeg from the executable's own location;
    // without this one falls through to system PATH and fails at spawn time.
    crate::runtime::ffmpeg::init_headless();
    // The login shell answers on its own thread long before an agent turn
    // needs a user-installed CLI.
    crate::runtime::user_path::prefetch();

    // A process that will not sync opens the database without the SDK:
    // loading its core extension is not free, and a library nobody is
    // replicating has no use for an upload queue.
    let (db, connections) = if sync {
        let (db, connections) = open_app_db_at(storage.path()).await?;
        (db, Some(connections))
    } else {
        (init_app_db_at(storage.path()).await?, None)
    };
    let state_db = init_state_db_at(storage.path()).await?;

    // Until admission is armed every `auth_visible_*` view is empty, so a
    // host that skipped this would show an empty library and no error.
    let mut fixture_principal = None;
    let (principal, lapsed) = match identity {
        Identity::Fixture(principal) => {
            auth::arm_write_admission(&db.0, Some(&principal)).await?;
            fixture_principal = Some(principal.clone());
            (Some(principal), None)
        }
        Identity::Stored => (
            auth::bootstrap_headless_admission(&db.0, &state_db.0).await?,
            None,
        ),
        // Signed out is an armed admission with nobody in it: the state a
        // sign-in starts from. Arming it explicitly covers the state the
        // bootstrap leaves closed: a committed sign-out still waiting for a
        // renderer to consume its journal, which the next sign-in's identity
        // switch consumes.
        Identity::StoredOrSignedOut => {
            match auth::bootstrap_headless_admission(&db.0, &state_db.0).await {
                Ok(Some(principal)) => (Some(principal), None),
                Ok(None) => {
                    auth::arm_write_admission(&db.0, None).await?;
                    (None, None)
                }
                Err(error) => {
                    eprintln!("[luma] the stored session no longer proves anyone: {error}");
                    auth::arm_write_admission(&db.0, None).await?;
                    (None, Some(error))
                }
            }
        }
    };

    let workspaces = Arc::new(match cache_dir {
        // Resolution is lazy: a machine with no venv yet still opens.
        Some(cache_dir) => headless_env::workspace_service(&storage, cache_dir),
        None => PythonWorkspaceService::new(
            storage.agent_workspaces_dir(),
            Arc::new(|| Err("this host runs no Python".to_string())),
        ),
    });
    let state_pool = state_db.0.clone();
    let mut services = AppServices::headless(db, state_db, storage, fixtures_root, workspaces)
        .with_events(events.clone())
        .with_fixture_principal(fixture_principal);

    if let Some(connections) = connections {
        match crate::sync::service::Service::open(connections, state_pool, events).await {
            Ok(replica) => services = services.with_sync(replica),
            Err(error) => eprintln!("[luma] record sync did not start: {error}"),
        }
    }
    if output {
        services = services.with_output().await?;
    }
    if audio {
        services = services.with_audio_device();
    }

    // Two subsystems keep their own copy of the settings they read and are
    // only ever told by a write; skipped, the session runs on the defaults.
    services
        .apply_persisted_settings()
        .await
        .map_err(|error| error.to_string())?;
    // Before any turn runs here. Not a boot failure: the rows wait for the
    // next one.
    if let Err(error) = services.recover_open_agent_messages().await {
        eprintln!("[luma] agent rows in progress were not recovered: {error}");
    }

    Ok(Booted {
        services,
        principal,
        lapsed,
    })
}
