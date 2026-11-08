//! Where the user's own command-line tools live, and the environment they
//! expect.
//!
//! An app launched from Finder, the Dock, launchd or a desktop launcher
//! inherits the system's minimal PATH (`/usr/bin:/bin:/usr/sbin:/sbin` on
//! macOS) and none of the user's shell setup: nothing under Homebrew, bun, npm,
//! nvm, volta, pnpm, cargo, asdf or mise. So, like VS Code, Zed and Zeron, Luma
//! asks the user's login shell for its environment once, and searches that
//! PATH (then a few well-known install directories) for user-installed CLIs:
//! codex, claude, a system ffmpeg. A CLI child gets the same PATH, so the
//! `node` a `#!/usr/bin/env node` script names is found too, plus the few
//! variables shell init owns that it needs ([`child_env_defaults`]).
//!
//! The tools Luma bundles are found from the resource directory instead, and
//! sandboxed workers set their own PATH on purpose; neither goes through here.
//!
//! The snapshot is taken once per process and cached, a failure included, and
//! is defensive about hostile shell init:
//! - `-l -i -c` first (interactive login: nvm and friends load in rc files),
//!   falling back to `-l -c` when that yields nothing (some rc files hang or
//!   `exec` a multiplexer when interactive). fish gets `-l -c`, csh `-c`.
//! - The shell runs in a session of its own, with no controlling terminal and
//!   stdin at `/dev/null`, so it can neither take over a terminal nor wait on
//!   input.
//! - Its output is read on a side thread, which answers as soon as the end
//!   marker appears: init that blocks after printing, or a grandchild holding
//!   the pipe, cannot wedge the read.
//! - Each attempt has a hard timeout that kills the shell's process group, and
//!   the candidate shells (`$SHELL`, the passwd entry, the platform defaults)
//!   share one overall budget. macOS falls back to launchd's PATH when no
//!   shell answers.
//!
//! `LUMA_NO_LOGIN_SHELL=1` disables the snapshot.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Variables besides PATH that shell init commonly owns and a CLI child
/// needs: TLS trust for corporate proxies (OpenSSL, Node, Python), the SSH
/// agent for git over SSH, the locale, and mise's record of the PATH entries
/// it added (it must travel with the shell's PATH, or `mise x` can rebuild
/// PATH behind a same-named wrapper that then re-runs itself).
#[cfg_attr(not(unix), allow(dead_code))]
const CAPTURED_VARS: [&str; 9] = [
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
    "SSH_AUTH_SOCK",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "__MISE_DIFF",
];

const LOCALE_VARS: [&str; 3] = ["LANG", "LC_ALL", "LC_CTYPE"];

/// What the login shell reported.
#[derive(Debug, Default, PartialEq)]
#[cfg_attr(not(unix), allow(dead_code))]
struct Snapshot {
    path: Option<OsString>,
    vars: Vec<(OsString, OsString)>,
}

struct UserEnv {
    search_path: OsString,
    vars: Vec<(OsString, OsString)>,
}

static USER_ENV: OnceLock<UserEnv> = OnceLock::new();

fn user_env() -> &'static UserEnv {
    USER_ENV.get_or_init(|| {
        let snapshot = login_snapshot();
        let own = std::env::var_os("PATH");
        let search_path = compose(
            own.as_deref(),
            snapshot.as_ref().and_then(|s| s.path.as_deref()),
            known_dirs(dirs::home_dir().as_deref()),
        );
        UserEnv {
            search_path,
            vars: snapshot.map(|s| s.vars).unwrap_or_default(),
        }
    })
}

/// Take the snapshot on a background thread, so the first CLI spawn finds it
/// ready instead of waiting on the shell. Call once at startup.
pub fn prefetch() {
    if USER_ENV.get().is_some() {
        return;
    }
    if let Err(error) = std::thread::Builder::new()
        .name("login-shell-env".into())
        .spawn(|| {
            user_env();
        })
    {
        eprintln!("[user-path] could not start the login shell reader: {error}");
    }
}

/// The directories searched for a user-installed CLI, as a PATH value, and
/// the PATH a CLI child runs with. Each directory appears once.
///
/// The first call waits for the login shell (bounded by the overall budget)
/// unless [`prefetch`] already did.
pub fn search_path() -> &'static OsStr {
    &user_env().search_path
}

/// The first executable called `name` on [`search_path`].
pub fn find(name: &str) -> Option<PathBuf> {
    find_in(name, search_path())
}

/// The first executable called `name` in the directories of `path`. Always
/// `None` on Windows, which resolves a bare name itself (with PATHEXT) and
/// keeps doing so.
pub fn find_in(name: &str, path: &OsStr) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    }
    #[cfg(not(unix))]
    {
        let _ = (name, path);
        None
    }
}

/// Variables a CLI child should get because this process lacks them: the
/// login shell's values for [`CAPTURED_VARS`], and on macOS a UTF-8
/// `LC_CTYPE` when a Finder launch left no locale at all (Node and Python
/// otherwise fall back to ASCII for file names and output).
pub fn child_env_defaults() -> Vec<(OsString, OsString)> {
    defaults_for(
        &user_env().vars,
        |key| std::env::var_os(key).is_some_and(|value| !value.is_empty()),
        cfg!(target_os = "macos"),
    )
}

fn defaults_for(
    captured: &[(OsString, OsString)],
    present: impl Fn(&OsStr) -> bool,
    macos: bool,
) -> Vec<(OsString, OsString)> {
    let mut defaults: Vec<(OsString, OsString)> = captured
        .iter()
        .filter(|(key, value)| !value.is_empty() && !present(key))
        .cloned()
        .collect();
    let has_locale =
        |key: &str| present(OsStr::new(key)) || defaults.iter().any(|(k, _)| k.as_os_str() == key);
    if macos && !LOCALE_VARS.iter().any(|key| has_locale(key)) {
        defaults.push(("LC_CTYPE".into(), "en_US.UTF-8".into()));
    }
    defaults
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Directories of a bare system PATH: launchd's for a Finder launch,
/// systemd's default for a service, and macOS's `path_helper` additions.
const SYSTEM_PATH_DIRS: &[&str] = &[
    "/usr/local/sbin",
    "/usr/local/bin",
    "/usr/sbin",
    "/usr/bin",
    "/sbin",
    "/bin",
    "/snap/bin",
    "/usr/games",
    "/usr/local/games",
    "/System/Cryptexes/App/usr/bin",
    "/Library/Apple/usr/bin",
];

/// Whether `path` is only the system's: what a GUI launch inherits.
fn is_bare_system_path(path: Option<&OsStr>) -> bool {
    path.is_none_or(|path| {
        std::env::split_paths(path).all(|dir| {
            dir.as_os_str().is_empty()
                || SYSTEM_PATH_DIRS
                    .iter()
                    .any(|system| dir == Path::new(system))
                || dir.starts_with("/var/run/com.apple.security.cryptexd")
        })
    })
}

/// The search path in precedence order. The login shell's PATH goes before
/// this process's only when ours is a bare system PATH, whose `/usr/bin`
/// stubs would otherwise shadow the user's toolchain; a PATH set up on purpose
/// (`dev.py` from a terminal, a venv, direnv) keeps precedence. The known
/// install directories come last, for a shell that never answered.
fn compose(own: Option<&OsStr>, login: Option<&OsStr>, known: Vec<PathBuf>) -> OsString {
    let split = |path: Option<&OsStr>| -> Vec<PathBuf> {
        path.map(|path| std::env::split_paths(path).collect())
            .unwrap_or_default()
    };
    let mut dirs = if cfg!(unix) && is_bare_system_path(own) {
        [split(login), split(own)].concat()
    } else {
        [split(own), split(login)].concat()
    };
    dirs.extend(known);
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|dir| {
        let key: PathBuf = dir.components().collect();
        !key.as_os_str().is_empty() && seen.insert(key)
    });
    std::env::join_paths(dirs).unwrap_or_default()
}

/// Where the common installers put CLIs, for when the login shell gave
/// nothing: native installers, bun, volta, npm's global prefix, pnpm, nvm's
/// node versions (newest first) and Homebrew. Missing ones are skipped.
fn known_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    if cfg!(windows) {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    if let Some(home) = home {
        for dir in [
            ".local/bin",
            ".bun/bin",
            ".volta/bin",
            ".npm-global/bin",
            "Library/pnpm",
            ".local/share/pnpm",
        ] {
            dirs.push(home.join(dir));
        }
        if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path().join("bin"))
                .collect();
            versions.sort_by_key(|dir| std::cmp::Reverse(node_version(dir)));
            dirs.extend(versions);
        }
    }
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.retain(|dir| dir.is_dir());
    dirs
}

/// `…/v22.13.0/bin` as `(22, 13, 0)`, so `v22` sorts above `v9`.
fn node_version(bin: &Path) -> Vec<u64> {
    bin.parent()
        .and_then(Path::file_name)
        .map(|name| {
            name.to_string_lossy()
                .trim_start_matches('v')
                .split('.')
                .map(|part| part.parse().unwrap_or(0))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(unix)]
fn login_snapshot() -> Option<Snapshot> {
    if std::env::var_os("LUMA_NO_LOGIN_SHELL").is_some_and(|value| !value.is_empty()) {
        eprintln!("[user-path] LUMA_NO_LOGIN_SHELL is set; not asking the login shell");
        return None;
    }
    let snapshot = unix::capture(&unix::user_shells(), unix::TOTAL_BUDGET);
    #[cfg(target_os = "macos")]
    let snapshot = snapshot.or_else(|| {
        let path = unix::launchctl_path()?;
        eprintln!("[user-path] no login shell answered; using launchd's PATH");
        Some(Snapshot {
            path: Some(path),
            vars: Vec::new(),
        })
    });
    if snapshot.is_none() {
        eprintln!("[user-path] no login shell answered; searching known install directories");
    }
    snapshot
}

#[cfg(not(unix))]
fn login_snapshot() -> Option<Snapshot> {
    None
}

/// The search path a launch with PATH `own` gets when `shell` is the user's
/// login shell and no install directory is known: the whole snapshot path
/// without the process-global cache.
#[cfg(all(test, unix))]
pub(crate) fn search_path_for(shell: &Path, own: &OsStr) -> OsString {
    let snapshot = unix::capture(&[shell.to_path_buf()], unix::TOTAL_BUDGET);
    compose(
        Some(own),
        snapshot.as_ref().and_then(|s| s.path.as_deref()),
        Vec::new(),
    )
}

#[cfg(unix)]
mod unix {
    use super::{Snapshot, CAPTURED_VARS};
    use std::ffi::{OsStr, OsString};
    use std::io::Read;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::process::CommandExt;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const BEGIN: &str = "__LUMA_SHELL_ENV_BEGIN__";
    const END: &str = "__LUMA_SHELL_ENV_END__";
    /// Enough for any sane environment; a runaway rc file cannot exhaust memory.
    const MAX_OUTPUT: usize = 2 * 1024 * 1024;
    const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
    /// All candidate shells together: the first CLI spawn may wait this long.
    pub(super) const TOTAL_BUDGET: Duration = Duration::from_secs(10);

    /// The first snapshot one of `shells` yields within `budget`.
    pub(super) fn capture(shells: &[PathBuf], budget: Duration) -> Option<Snapshot> {
        let deadline = Instant::now() + budget;
        for shell in shells {
            if Instant::now() >= deadline {
                break;
            }
            match snapshot_shell(shell, ATTEMPT_TIMEOUT, deadline) {
                Some(snapshot) => {
                    eprintln!(
                        "[user-path] login shell {} PATH: {}",
                        shell.display(),
                        snapshot
                            .path
                            .as_deref()
                            .unwrap_or_default()
                            .to_string_lossy()
                    );
                    return Some(snapshot);
                }
                None => eprintln!(
                    "[user-path] login shell {} reported no environment",
                    shell.display()
                ),
            }
        }
        None
    }

    /// The user's shells, most specific first: `$SHELL`, the passwd entry
    /// (a launchd or systemd launch may have no SHELL), then the defaults.
    /// Missing, non-executable and nologin shells are skipped.
    pub(super) fn user_shells() -> Vec<PathBuf> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        candidates.extend(
            std::env::var_os("SHELL")
                .filter(|shell| !shell.is_empty())
                .map(PathBuf::from),
        );
        candidates.extend(passwd_shell());
        candidates.extend(["/bin/zsh", "/bin/bash", "/bin/sh"].map(PathBuf::from));
        let mut seen = std::collections::HashSet::new();
        candidates.retain(|shell| {
            let blocked = matches!(
                shell.file_name().and_then(OsStr::to_str),
                Some("nologin" | "false") | None
            );
            !blocked && super::is_executable(shell) && seen.insert(shell.clone())
        });
        candidates
    }

    fn passwd_shell() -> Option<PathBuf> {
        // SAFETY: getpwuid's static buffer is read here only, under the
        // OnceLock that serializes the snapshot.
        unsafe {
            let entry = libc::getpwuid(libc::getuid());
            if entry.is_null() || (*entry).pw_shell.is_null() {
                return None;
            }
            let shell = std::ffi::CStr::from_ptr((*entry).pw_shell).to_bytes();
            (!shell.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(shell)))
        }
    }

    fn shell_name(shell: &Path) -> &str {
        shell
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
    }

    /// Flag sets to try, most loaded first. csh and tcsh reject `-l` with
    /// `-c`; fish reads config.fish on every start, so it needs no `-i`.
    fn flag_sets(shell: &Path) -> &'static [&'static [&'static str]] {
        match shell_name(shell) {
            "csh" | "tcsh" => &[&["-c"]],
            "fish" => &[&["-l", "-c"], &["-c"]],
            _ => &[&["-l", "-i", "-c"], &["-l", "-c"]],
        }
    }

    /// The probe in the shell's own language. Nushell discards non-final
    /// values (so `echo` prints nothing) and has no `env` builtin. `env` is
    /// named by path: the PATH an rc file builds need not include `/usr/bin`.
    fn script(shell: &Path) -> String {
        if shell_name(shell) == "nu" {
            format!("print '{BEGIN}'; ^/usr/bin/env; print '{END}'")
        } else {
            format!("echo {BEGIN}; /usr/bin/env; echo {END}")
        }
    }

    /// Run the probe per flag set until one yields a PATH. Each attempt gets
    /// `timeout`, cut short by the shared `deadline`.
    pub(super) fn snapshot_shell(
        shell: &Path,
        timeout: Duration,
        deadline: Instant,
    ) -> Option<Snapshot> {
        let script = script(shell);
        for flags in flag_sets(shell) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Some(snapshot) = run(shell, flags, &script, timeout.min(remaining))
                .as_deref()
                .and_then(parse)
            {
                return Some(snapshot);
            }
        }
        None
    }

    /// Spawn the shell and collect its stdout until the end marker, EOF, or
    /// `timeout`, which kills the shell's whole session. `None` if it could
    /// not start or did not answer in time.
    fn run(shell: &Path, flags: &[&str], script: &str, timeout: Duration) -> Option<Vec<u8>> {
        let mut cmd = crate::runtime::command::command(shell);
        cmd.args(flags)
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            // Lets an rc file skip slow setup for this probe, like VS Code's
            // VSCODE_RESOLVING_ENVIRONMENT; TERM=dumb quiets fancy prompts.
            .env("LUMA_RESOLVING_ENVIRONMENT", "1")
            .env("TERM", "dumb");
        // SAFETY: setsid is async-signal-safe. A new session has no
        // controlling terminal, so an interactive shell's job control cannot
        // take a terminal's foreground from the Luma that launched it.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd.spawn().ok()?;
        let mut stdout = child.stdout.take()?;
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("login-shell-env-read".into())
            .spawn(move || {
                let mut output = Vec::new();
                let mut chunk = [0u8; 8192];
                // Stop at the end marker rather than EOF: something init
                // started in the background may hold stdout open forever.
                while find(&output, END.as_bytes()).is_none() && output.len() < MAX_OUTPUT {
                    match stdout.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => output.extend_from_slice(&chunk[..read]),
                    }
                }
                let _ = send.send(output);
            })
            .ok()?;
        let output = receive.recv_timeout(timeout).ok();
        if output.is_none() {
            // SAFETY: the shell leads the fresh session created for it and
            // has not been reaped, so the group id is still its own.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
        }
        // Reap off this thread: a shell that answered may still be finishing.
        let _ = std::thread::Builder::new()
            .name("login-shell-env-reap".into())
            .spawn(move || {
                let _ = child.wait();
            });
        output
    }

    /// launchd's own PATH (`launchctl setenv`, a LaunchAgent): what a Finder
    /// launch would have inherited had the user configured one.
    #[cfg(target_os = "macos")]
    pub(super) fn launchctl_path() -> Option<OsString> {
        let mut child = std::process::Command::new("/bin/launchctl")
            .args(["getenv", "PATH"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().ok()?.is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut output = Vec::new();
        child.stdout.take()?.read_to_end(&mut output).ok()?;
        let path = output.trim_ascii();
        (!path.is_empty()).then(|| OsString::from_vec(path.to_vec()))
    }

    /// PATH and [`CAPTURED_VARS`] from the `env` dump between the last end
    /// marker and the last begin marker before it: rc noise, or a marker init
    /// echoed itself, lands before the real one. No PATH means no snapshot.
    pub(super) fn parse(output: &[u8]) -> Option<Snapshot> {
        let end = rfind(output, END.as_bytes())?;
        let begin = rfind(&output[..end], BEGIN.as_bytes())?;
        let dump = &output[begin + BEGIN.len()..end];
        let mut snapshot = Snapshot::default();
        for line in dump.split(|byte| *byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let Some(eq) = line.iter().position(|byte| *byte == b'=') else {
                continue;
            };
            let (key, value) = (&line[..eq], &line[eq + 1..]);
            if value.is_empty() {
                continue;
            }
            if key == b"PATH" {
                snapshot
                    .path
                    .get_or_insert_with(|| OsString::from_vec(value.to_vec()));
            } else if CAPTURED_VARS.iter().any(|name| name.as_bytes() == key)
                && !snapshot.vars.iter().any(|(k, _)| k.as_bytes() == key)
            {
                snapshot.vars.push((
                    OsString::from_vec(key.to_vec()),
                    OsString::from_vec(value.to_vec()),
                ));
            }
        }
        snapshot.path.is_some().then_some(snapshot)
    }

    fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .rposition(|window| window == needle)
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn fake_shell(dir: &Path, body: &str) -> PathBuf {
            let path = dir.join("fake-shell");
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        /// The tail of a fake shell: skip the flags, run the `-c` payload.
        const RUN_PAYLOAD: &str = r#"
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-c" ]; then shift; exec /bin/sh -c "$1"; fi
  shift
done
exit 1
"#;

        fn snapshot_path(shell: &Path, timeout: Duration) -> Option<OsString> {
            snapshot_shell(shell, timeout, Instant::now() + timeout * 4)?.path
        }

        #[test]
        fn parses_the_path_between_the_markers_through_rc_noise() {
            let output = format!(
                "Last login: today\n\x1b[1mwelcome\x1b[0m\n{BEGIN}\nHOME=/home/u\nPATH=/custom/bin:/usr/bin\nX=y\n{END}\ntrailing {BEGIN}"
            );
            assert_eq!(
                parse(output.as_bytes()).unwrap().path.unwrap(),
                OsString::from("/custom/bin:/usr/bin")
            );
        }

        #[test]
        fn a_marker_echoed_by_init_does_not_shadow_the_real_dump() {
            let output = format!("{BEGIN}\ngarbage\n{BEGIN}\nPATH=/real/bin\n{END}\n");
            assert_eq!(
                parse(output.as_bytes()).unwrap().path.unwrap(),
                OsString::from("/real/bin")
            );
        }

        #[test]
        fn output_without_both_markers_or_a_path_is_no_snapshot() {
            assert!(parse(b"PATH=/usr/bin\n").is_none());
            assert!(parse(format!("{BEGIN}\nPATH=/usr/bin\n").as_bytes()).is_none());
            assert!(parse(format!("{END}\nPATH=/usr/bin\n{BEGIN}").as_bytes()).is_none());
            assert!(parse(format!("{BEGIN}\nLANG=C\nPATH=\n{END}").as_bytes()).is_none());
        }

        #[test]
        fn keeps_tls_ssh_and_locale_variables_and_nothing_else() {
            let output = format!(
                "{BEGIN}\nPATH=/bin\nSSL_CERT_FILE=/etc/corp.pem\nNODE_EXTRA_CA_CERTS=/etc/corp.pem\n\
                 SSH_AUTH_SOCK=/tmp/agent.sock\nLANG=de_DE.UTF-8\nSECRET_TOKEN=nope\nSSL_CERT_DIR=\n{END}\n"
            );
            let kept: Vec<_> = parse(output.as_bytes())
                .unwrap()
                .vars
                .iter()
                .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
                .collect();
            assert_eq!(
                kept,
                [
                    "SSL_CERT_FILE=/etc/corp.pem",
                    "NODE_EXTRA_CA_CERTS=/etc/corp.pem",
                    "SSH_AUTH_SOCK=/tmp/agent.sock",
                    "LANG=de_DE.UTF-8",
                ]
            );
        }

        #[test]
        fn shells_get_flags_and_a_probe_they_accept() {
            assert_eq!(flag_sets(Path::new("/bin/tcsh")), &[&["-c"][..]]);
            assert_eq!(
                flag_sets(Path::new("/opt/homebrew/bin/fish"))[0],
                &["-l", "-c"]
            );
            assert_eq!(flag_sets(Path::new("/bin/zsh"))[0], &["-l", "-i", "-c"]);
            assert!(script(Path::new("/opt/homebrew/bin/nu")).contains("^/usr/bin/env"));
            assert!(script(Path::new("/bin/zsh")).starts_with("echo "));
        }

        #[test]
        fn reads_the_path_a_noisy_shell_exports() {
            let dir = tempfile::tempdir().unwrap();
            let shell = fake_shell(
                dir.path(),
                &format!(
                    "#!/bin/sh\necho 'rc: loading plugins'\necho 'rc warning' >&2\n\
                     PATH=\"/luma-test/custom/bin:/usr/bin:/bin\"; export PATH\n{RUN_PAYLOAD}"
                ),
            );
            assert_eq!(
                snapshot_path(&shell, Duration::from_secs(10)).unwrap(),
                OsString::from("/luma-test/custom/bin:/usr/bin:/bin")
            );
        }

        #[test]
        fn the_shell_runs_in_its_own_session() {
            let dir = tempfile::tempdir().unwrap();
            let shell = fake_shell(
                dir.path(),
                &format!(
                    "#!/bin/sh\nsid=$(python3 -c 'import os; print(os.getsid(0))')\n\
                     echo {BEGIN}; echo \"PATH=/sid/$sid\"; echo {END}\n"
                ),
            );
            let path = snapshot_path(&shell, Duration::from_secs(10)).unwrap();
            let sid: i32 = path
                .to_string_lossy()
                .strip_prefix("/sid/")
                .and_then(|sid| sid.trim().parse().ok())
                .expect("session id reported");
            // SAFETY: getsid(0) only reads this process's own session id.
            assert_ne!(sid, unsafe { libc::getsid(0) });
        }

        #[test]
        fn a_missing_or_failing_shell_yields_nothing_and_the_next_one_answers() {
            let dir = tempfile::tempdir().unwrap();
            let failing = dir.path().join("failing");
            std::fs::write(&failing, "#!/bin/sh\necho 'rc: syntax error'\nexit 1\n").unwrap();
            std::fs::set_permissions(&failing, std::fs::Permissions::from_mode(0o755)).unwrap();
            let working = fake_shell(
                dir.path(),
                &format!("#!/bin/sh\nPATH=/luma-test/next/bin; export PATH\n{RUN_PAYLOAD}"),
            );
            let missing = dir.path().join("no-such-shell");
            assert!(capture(std::slice::from_ref(&missing), Duration::from_secs(10)).is_none());
            assert!(capture(std::slice::from_ref(&failing), Duration::from_secs(10)).is_none());
            let snapshot = capture(&[missing, failing, working], Duration::from_secs(10)).unwrap();
            assert_eq!(
                snapshot.path.unwrap(),
                OsString::from("/luma-test/next/bin")
            );
        }

        #[test]
        fn falls_back_to_a_non_interactive_login_when_interactive_init_hangs() {
            let dir = tempfile::tempdir().unwrap();
            // rc files that wedge only when interactive (`exec tmux` and
            // friends): sleep whenever -i is passed.
            let shell = fake_shell(
                dir.path(),
                &format!(
                    "#!/bin/sh\ncase \" $* \" in *\" -i \"*) sleep 60;; esac\n\
                     PATH=\"/luma-test/fallback/bin:/usr/bin\"; export PATH\n{RUN_PAYLOAD}"
                ),
            );
            let path = snapshot_path(&shell, Duration::from_secs(3)).unwrap();
            assert_eq!(path, OsString::from("/luma-test/fallback/bin:/usr/bin"));
        }

        #[test]
        fn a_shell_that_never_answers_is_killed_with_its_children() {
            let dir = tempfile::tempdir().unwrap();
            let pids = dir.path().join("pids");
            let shell = fake_shell(
                dir.path(),
                &format!("#!/bin/sh\necho $$ >> '{}'\nsleep 60\n", pids.display()),
            );
            assert!(snapshot_path(&shell, Duration::from_millis(300)).is_none());
            let pids = std::fs::read_to_string(&pids).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            for pid in pids.lines().map(|pid| pid.parse::<i32>().unwrap()) {
                // SAFETY: signal zero only probes whether the group is gone.
                while unsafe { libc::kill(-pid, 0) } == 0 {
                    assert!(
                        Instant::now() < deadline,
                        "a timed-out shell's session survived"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }

        #[test]
        fn the_shells_share_one_budget() {
            let dir = tempfile::tempdir().unwrap();
            let shell = fake_shell(dir.path(), "#!/bin/sh\nsleep 60\n");
            let shells = vec![shell; 3];
            let started = Instant::now();
            assert!(capture(&shells, Duration::from_millis(400)).is_none());
            // Three shells with two attempts each at the per-attempt timeout
            // would take 30s; the budget cuts them all to its own length.
            assert!(started.elapsed() < ATTEMPT_TIMEOUT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(values: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        values
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[cfg(unix)]
    fn executable(path: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_path_buf()
    }

    #[cfg(unix)]
    #[test]
    fn finds_the_first_executable_on_the_search_path() {
        let dir = tempfile::tempdir().unwrap();
        let (bun, brew) = (dir.path().join("bun"), dir.path().join("brew"));
        let codex = executable(&bun.join("codex"));
        executable(&brew.join("codex"));
        let tool = executable(&brew.join("tool"));
        // Neither a plain file nor a directory is a candidate.
        std::fs::write(bun.join("tool"), "").unwrap();
        std::fs::create_dir_all(bun.join("dir")).unwrap();

        let path = std::env::join_paths([Path::new("relative"), &bun, &brew]).unwrap();
        assert_eq!(find_in("codex", &path), Some(codex));
        assert_eq!(find_in("tool", &path), Some(tool));
        assert_eq!(find_in("dir", &path), None);
        assert_eq!(find_in("missing", &path), None);
        assert_eq!(find_in("codex", OsStr::new("")), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_login_path_leads_only_behind_a_bare_system_path() {
        let known = vec![PathBuf::from("/home/me/.bun/bin")];
        let login = OsStr::new("/opt/homebrew/bin:/usr/bin");
        assert_eq!(
            compose(
                Some(OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
                Some(login),
                known.clone()
            ),
            OsString::from("/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:/home/me/.bun/bin")
        );
        assert_eq!(
            compose(
                Some(OsStr::new("/venv/bin:/usr/bin")),
                Some(login),
                known.clone()
            ),
            OsString::from("/venv/bin:/usr/bin:/opt/homebrew/bin:/home/me/.bun/bin")
        );
        assert_eq!(
            compose(None, None, known),
            OsString::from("/home/me/.bun/bin")
        );
        assert_eq!(
            compose(Some(OsStr::new("/a/:/a::/b")), None, Vec::new()),
            OsString::from("/a/:/b")
        );
    }

    #[cfg(unix)]
    #[test]
    fn known_dirs_list_what_exists_with_the_newest_node_first() {
        let home = tempfile::tempdir().unwrap();
        for dir in [
            ".bun/bin",
            ".nvm/versions/node/v9.11.2/bin",
            ".nvm/versions/node/v22.13.0/bin",
        ] {
            std::fs::create_dir_all(home.path().join(dir)).unwrap();
        }
        let dirs = known_dirs(Some(home.path()));
        let mine: Vec<_> = dirs
            .iter()
            .filter_map(|dir| dir.strip_prefix(home.path()).ok())
            .collect();
        assert_eq!(
            mine,
            [
                Path::new(".bun/bin"),
                Path::new(".nvm/versions/node/v22.13.0/bin"),
                Path::new(".nvm/versions/node/v9.11.2/bin"),
            ]
        );
    }

    #[test]
    fn captured_variables_fill_only_what_this_process_lacks() {
        let captured = pairs(&[
            ("SSL_CERT_FILE", "/etc/corp.pem"),
            ("SSH_AUTH_SOCK", "/tmp/shell-agent"),
        ]);
        let ours = |key: &OsStr| key == "SSH_AUTH_SOCK" || key == "LANG";
        assert_eq!(
            defaults_for(&captured, ours, true),
            pairs(&[("SSL_CERT_FILE", "/etc/corp.pem")])
        );
    }

    #[test]
    fn macos_gets_a_utf8_ctype_only_without_any_locale() {
        assert_eq!(
            defaults_for(&[], |_| false, true),
            pairs(&[("LC_CTYPE", "en_US.UTF-8")])
        );
        assert_eq!(
            defaults_for(&pairs(&[("LANG", "fr_FR.UTF-8")]), |_| false, true),
            pairs(&[("LANG", "fr_FR.UTF-8")])
        );
        assert!(defaults_for(&[], |key| key == "LC_ALL", true).is_empty());
        assert!(defaults_for(&[], |_| false, false).is_empty());
    }
}
