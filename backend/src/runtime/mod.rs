//! The bundled runtimes the backend runs other programs on: Python and its
//! environment, ffmpeg, how a child process is started, and where the user's
//! own CLIs are found.

pub(crate) mod command;
pub(crate) mod ffmpeg;
pub mod python;
pub(crate) mod user_path;
