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
