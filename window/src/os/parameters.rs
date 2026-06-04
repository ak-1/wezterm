use wezterm_color_types::LinearRgba;
use wezterm_font::parser::ParsedFont;

use crate::ULength;

pub type FontAndSize = (ParsedFont, f64);

#[derive(Default, Clone, Debug)]
pub struct TitleBar {
    pub padding_left: ULength,
    pub padding_right: ULength,
    pub height: Option<ULength>,
    pub font_and_size: Option<FontAndSize>,
}

#[derive(Default, Clone, Debug)]
pub struct Border {
    pub top: ULength,
    pub left: ULength,
    pub bottom: ULength,
    pub right: ULength,
    pub color: LinearRgba,
}

#[derive(Default, Clone, Debug)]
pub struct Parameters {
    pub title_bar: TitleBar,
    /// If present, the application should draw it
    pub border_dimensions: Option<Border>,
    /// When true, the window uses client-side decorations and the
    /// windowing system expects the application to initiate interactive
    /// resizes itself. The GUI responds by providing an internal resize
    /// border that calls [`crate::WindowOps::request_drag_resize`].
    pub client_side_resize: bool,
}
