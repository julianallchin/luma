#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::current_platform;

#[cfg(all(feature = "test-support", any(feature = "wayland", feature = "x11")))]
mod headless_renderer;
#[cfg(all(feature = "test-support", any(feature = "wayland", feature = "x11")))]
pub use headless_renderer::headless_renderer;
