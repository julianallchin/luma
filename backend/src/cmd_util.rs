use std::process::Command;

/// Hide the console window that Windows shows for child processes.
/// No-op on non-Windows platforms.
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
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
