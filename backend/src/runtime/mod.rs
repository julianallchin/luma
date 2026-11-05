//! The bundled runtimes the backend runs other programs on: Python and its
//! environment, ffmpeg, and how a child process is started.

pub(crate) mod command;
pub(crate) mod ffmpeg;
pub mod python;
