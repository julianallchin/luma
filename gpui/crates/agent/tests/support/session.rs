//! The session fixture lives in the library now (`gpui_agent::fixture::session`)
//! so the script runner seeds the same way. The cargo suites that still seed a
//! stored session by hand (`headless/signin.rs`, `app_pixel/venues_pixels.rs`)
//! reach it through `support::session`.

#[allow(unused_imports)] // each binary uses a different subset
pub use gpui_agent::fixture::session::*;
