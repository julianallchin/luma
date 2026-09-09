//! Rounded Comet-style toggles and segmented controls.
use crate::float;
use gpui::*;

pub fn luma_toggle(label: &str, pressed: bool) -> Div {
    float::segment(label.to_string(), pressed, label.to_string())
        .flex_none()
        .h(px(crate::CONTROL_HEIGHT))
}

pub fn luma_toggle_group(value: &str, options: &[&str]) -> Div {
    float::segmented().children(
        options
            .iter()
            .map(|option| float::segment(option.to_string(), *option == value, option.to_string())),
    )
}
