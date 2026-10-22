// Muzeeka colors, matched to the CSS variables in src/app.css.
//
// `accent` follows the playing track. Every call reads the current value, so a
// window redraw after `set_accent_rgb` picks the new color up without each
// control storing its own copy.

use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{rgb, rgba, Rgba};

/// Purple used when nothing is playing, or the cover has no usable color.
pub const DEFAULT_ACCENT: u32 = 0xcdcecd;
/// Same wash as the old `rgba(0x8b5cf61f)`.
const ACCENT_SOFT_ALPHA: f32 = 31.0 / 255.0;

static ACCENT: AtomicU32 = AtomicU32::new(DEFAULT_ACCENT);

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
    rgba(0x121212ab)
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
    rgb(ACCENT.load(Ordering::Relaxed))
}

pub fn accent_soft() -> Rgba {
    // Flattened onto the row background. A plain alpha wash lets the rows
    // behind a dragged row show through it.
    mix(bg_surface(), accent(), ACCENT_SOFT_ALPHA)
}

fn mix(base: Rgba, over: Rgba, amount: f32) -> Rgba {
    let t = amount.clamp(0.0, 1.0);
    rgb(channel(base.r, over.r, t) << 16
        | channel(base.g, over.g, t) << 8
        | channel(base.b, over.b, t))
}

fn channel(base: f32, over: f32, t: f32) -> u32 {
    (base + (over - base) * t)
        .clamp(0.0, 1.0)
        .mul_add(255.0, 0.5) as u32
}

/// Install `0xRRGGBB` as the accent. Returns whether it changed.
pub fn set_accent_rgb(rgb24: u32) -> bool {
    let next = rgb24 & 0x00ff_ffff;
    ACCENT.swap(next, Ordering::Relaxed) != next
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
