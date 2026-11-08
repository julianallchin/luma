use std::ffi::OsStr;
use std::process::Command;

/// A `Command` for a child tool (ffmpeg, Python, an agent CLI). Every spawn
/// goes through here so that Windows never flashes a console window for it.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// [`command`] for a child driven from async code.
pub fn async_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    tokio::process::Command::from(command(program))
}

/// Put the child in its own process group so the host can signal the child and
/// everything it spawns with one `killpg` (design §16.2). Windows job objects
/// are the equivalent primitive; until the Windows sandbox exists (design §17.6
/// says the Python tool does not ship there) this is a no-op.
pub fn new_process_group(cmd: &mut Command) -> &mut Command {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd
}

/// [`command`] for a child that runs below everything the user is looking at.
///
/// On macOS the child starts through `taskpolicy -b`, which puts it in the
/// Darwin background band (lowest CPU priority, throttled disk and network
/// I/O; in practice its GPU work also stops holding up the app's frames) and
/// execs it in place, so the pid is the program's. A wrapper, not a
/// `pre_exec` hook: any `pre_exec` closure makes std `fork()` this large,
/// many-threaded process instead of using `posix_spawn`. Other platforms run
/// the child at normal priority for now.
pub fn background_command(program: impl AsRef<OsStr>) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = command("/usr/sbin/taskpolicy");
        cmd.arg("-b").arg(program);
        cmd
    }
    #[cfg(not(target_os = "macos"))]
    {
        command(program)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The scheduling priority `ps` reports for a shell spawned from `cmd`.
    fn shell_priority(cmd: &mut Command) -> i32 {
        let out = cmd
            .args(["-c", "ps -o pri= -p $$"])
            .output()
            .expect("spawn sh");
        String::from_utf8(out.stdout)
            .expect("ps output is UTF-8")
            .trim()
            .parse()
            .expect("ps prints a priority")
    }

    #[test]
    fn a_background_child_runs_below_a_default_one() {
        let default = shell_priority(&mut command("/bin/sh"));
        let backgrounded = shell_priority(&mut background_command("/bin/sh"));
        assert!(
            backgrounded < default,
            "background child at priority {backgrounded}, default child at {default}"
        );
    }
}
