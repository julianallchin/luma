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
