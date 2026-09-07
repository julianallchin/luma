//! Nucleo UI outline icons embedded in the native app.
/// View/render settings.
pub fn eye() -> gpui::Svg {
    gpui::svg().data(include_bytes!("../assets/nucleo/eye.svg"))
}
/// Add an object.
pub fn plus() -> gpui::Svg {
    gpui::svg().data(include_bytes!("../assets/nucleo/plus.svg"))
}
