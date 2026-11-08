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

/// Run the child below everything the user is looking at. On macOS that is
/// the Darwin background band (`PRIO_DARWIN_BG`): the child and everything it
/// spawns get the lowest CPU priority and throttled disk and network I/O, and
/// in practice their GPU work stops holding up the app's frames. Other
/// platforms run the child at normal priority for now.
pub fn background(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: the closure runs in the forked child before exec and makes
        // one async-signal-safe syscall on that child.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpriority(libc::PRIO_DARWIN_PROCESS, 0, libc::PRIO_DARWIN_BG) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
    }
    cmd
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
        let backgrounded = shell_priority(background(&mut command("/bin/sh")));
        assert!(
            backgrounded < default,
            "background child at priority {backgrounded}, default child at {default}"
        );
    }
}
