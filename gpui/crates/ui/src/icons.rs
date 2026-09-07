//! Nucleo UI outline icons, embedded once for the app and both harnesses.
use gpui::{AssetSource, SharedString};
use gpui_component::IconNamed;
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconName {
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    BookOpen,
    Bot,
    Check,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    Close,
    Cpu,
    Network,
    PanelLeft,
    PanelRight,
    Play,
    Plus,
    SquareTerminal,
    Undo,
    RotateLeft,
    RotateRight,
}

impl IconNamed for IconName {
    fn path(self) -> SharedString {
        match self {
            Self::ArrowDown => "nucleo/arrow-down.svg".into(),
            Self::ArrowLeft => "nucleo/arrow-left.svg".into(),
            Self::ArrowRight => "nucleo/arrow-right.svg".into(),
            Self::ArrowUp => "nucleo/arrow-up.svg".into(),
            Self::BookOpen => "nucleo/book-open.svg".into(),
            Self::Bot => "nucleo/robot.svg".into(),
            Self::Check => "nucleo/check.svg".into(),
            Self::ChevronDown => "nucleo/chevron-down.svg".into(),
            Self::ChevronLeft => "nucleo/chevron-left.svg".into(),
            Self::ChevronRight => "nucleo/chevron-right.svg".into(),
            Self::ChevronUp => "nucleo/chevron-up.svg".into(),
            Self::Close => "nucleo/xmark.svg".into(),
            Self::Cpu => "nucleo/microchip.svg".into(),
            Self::Network => "nucleo/nodes.svg".into(),
            Self::PanelLeft => "nucleo/sidebar-left.svg".into(),
            Self::PanelRight => "nucleo/sidebar-right.svg".into(),
            Self::Play => "nucleo/media-play.svg".into(),
            Self::Plus => "nucleo/plus.svg".into(),
            Self::SquareTerminal => "nucleo/square-terminal.svg".into(),
            Self::Undo => "nucleo/undo.svg".into(),
            Self::RotateLeft => "nucleo/arrow-rotate-anticlockwise.svg".into(),
            Self::RotateRight => "nucleo/arrow-rotate-clockwise.svg".into(),
        }
    }
}

/// Native icons plus the assets used internally by third-party components.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "nucleo/arrow-down.svg" | "icons/arrow-down.svg" => {
                Some(include_bytes!("../assets/nucleo/arrow-down.svg"))
            }
            "nucleo/arrow-left.svg" | "icons/arrow-left.svg" => {
                Some(include_bytes!("../assets/nucleo/arrow-left.svg"))
            }
            "nucleo/arrow-right.svg" | "icons/arrow-right.svg" => {
                Some(include_bytes!("../assets/nucleo/arrow-right.svg"))
            }
            "nucleo/arrow-rotate-anticlockwise.svg" => Some(include_bytes!(
                "../assets/nucleo/arrow-rotate-anticlockwise.svg"
            )),
            "nucleo/arrow-rotate-clockwise.svg" => Some(include_bytes!(
                "../assets/nucleo/arrow-rotate-clockwise.svg"
            )),
            "nucleo/arrow-up.svg" | "icons/arrow-up.svg" => {
                Some(include_bytes!("../assets/nucleo/arrow-up.svg"))
            }
            "nucleo/book-open.svg" | "icons/book-open.svg" => {
                Some(include_bytes!("../assets/nucleo/book-open.svg"))
            }
            "nucleo/check.svg" | "icons/check.svg" => {
                Some(include_bytes!("../assets/nucleo/check.svg"))
            }
            "nucleo/chevron-down.svg" | "icons/chevron-down.svg" => {
                Some(include_bytes!("../assets/nucleo/chevron-down.svg"))
            }
            "nucleo/chevron-left.svg" | "icons/chevron-left.svg" => {
                Some(include_bytes!("../assets/nucleo/chevron-left.svg"))
            }
            "nucleo/chevron-right.svg" | "icons/chevron-right.svg" => {
                Some(include_bytes!("../assets/nucleo/chevron-right.svg"))
            }
            "nucleo/chevron-up.svg" | "icons/chevron-up.svg" => {
                Some(include_bytes!("../assets/nucleo/chevron-up.svg"))
            }
            "nucleo/eye.svg" | "icons/eye.svg" => Some(include_bytes!("../assets/nucleo/eye.svg")),
            "nucleo/media-play.svg" | "icons/play.svg" => {
                Some(include_bytes!("../assets/nucleo/media-play.svg"))
            }
            "nucleo/microchip.svg" | "icons/cpu.svg" => {
                Some(include_bytes!("../assets/nucleo/microchip.svg"))
            }
            "nucleo/nodes.svg" | "icons/network.svg" => {
                Some(include_bytes!("../assets/nucleo/nodes.svg"))
            }
            "nucleo/plus.svg" | "icons/plus.svg" => {
                Some(include_bytes!("../assets/nucleo/plus.svg"))
            }
            "nucleo/robot.svg" | "icons/bot.svg" => {
                Some(include_bytes!("../assets/nucleo/robot.svg"))
            }
            "nucleo/sidebar-left.svg" | "icons/panel-left.svg" => {
                Some(include_bytes!("../assets/nucleo/sidebar-left.svg"))
            }
            "nucleo/sidebar-right.svg" | "icons/panel-right.svg" => {
                Some(include_bytes!("../assets/nucleo/sidebar-right.svg"))
            }
            "nucleo/square-terminal.svg" | "icons/square-terminal.svg" => {
                Some(include_bytes!("../assets/nucleo/square-terminal.svg"))
            }
            "nucleo/undo.svg" | "icons/undo-2.svg" => {
                Some(include_bytes!("../assets/nucleo/undo.svg"))
            }
            "nucleo/xmark.svg" | "icons/close.svg" => {
                Some(include_bytes!("../assets/nucleo/xmark.svg"))
            }
            _ => None,
        };
        if let Some(bytes) = bytes {
            Ok(Some(Cow::Borrowed(bytes)))
        } else {
            gpui_component_assets::Assets.load(path)
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = gpui_component_assets::Assets.list(path)?;
        paths.extend(
            [
                "nucleo/arrow-down.svg",
                "nucleo/arrow-left.svg",
                "nucleo/arrow-right.svg",
                "nucleo/arrow-rotate-anticlockwise.svg",
                "nucleo/arrow-rotate-clockwise.svg",
                "nucleo/arrow-up.svg",
                "nucleo/book-open.svg",
                "nucleo/check.svg",
                "nucleo/chevron-down.svg",
                "nucleo/chevron-left.svg",
                "nucleo/chevron-right.svg",
                "nucleo/chevron-up.svg",
                "nucleo/eye.svg",
                "nucleo/media-play.svg",
                "nucleo/microchip.svg",
                "nucleo/nodes.svg",
                "nucleo/plus.svg",
                "nucleo/robot.svg",
                "nucleo/sidebar-left.svg",
                "nucleo/sidebar-right.svg",
                "nucleo/square-terminal.svg",
                "nucleo/undo.svg",
                "nucleo/xmark.svg",
            ]
            .into_iter()
            .filter(|name| name.starts_with(path))
            .map(SharedString::from),
        );
        Ok(paths)
    }
}

/// View/render settings.
pub fn eye() -> gpui::Svg {
    gpui::svg().data(include_bytes!("../assets/nucleo/eye.svg"))
}
/// Add an object.
pub fn plus() -> gpui::Svg {
    gpui::svg().data(include_bytes!("../assets/nucleo/plus.svg"))
}
