// Muzeeka colors, matched to the CSS variables in src/app.css.

use gpui::{Rgba, rgb, rgba};

pub const FONT_FAMILY: &str = "Google Sans Flex";

pub fn bg_deep() -> Rgba {
    rgb(0x0a0a0a)
}

pub fn bg_surface() -> Rgba {
    rgb(0x111111)
}

pub fn bg_elevated() -> Rgba {
    rgb(0x1a1a1a)
}

pub fn bg_glass() -> Rgba {
    rgb(0x121212)
}

pub fn hover() -> Rgba {
    rgb(0x202020)
}

/// Neutral foreground for primary text and idle controls: like, play, pause, volume.
pub fn text() -> Rgba {
    rgb(0xdcdce1)
}

/// Same neutral as [`text`], so secondary icons do not drift to another gray.
pub fn text_secondary() -> Rgba {
    rgb(0xdcdce1)
}

pub fn text_muted() -> Rgba {
    rgba(0x7e7e7ee6)
}

pub fn border() -> Rgba {
    rgba(0xffffff0f)
}

pub fn accent() -> Rgba {
    rgb(0x8b5cf6)
}

pub fn accent_soft() -> Rgba {
    rgba(0x8b5cf61f)
}

pub fn danger() -> Rgba {
    rgb(0xe81123)
}

pub fn white() -> Rgba {
    rgb(0xffffff)
}

/// Web `::-webkit-scrollbar-thumb`, 6px on a transparent track.
pub fn scrollbar_thumb() -> Rgba {
    rgb(0x1c1c1c)
}

pub fn scrollbar_thumb_hover() -> Rgba {
    rgb(0x262626)
}
