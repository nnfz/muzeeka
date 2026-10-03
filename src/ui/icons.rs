// Icons from src/ui/src/icons/svgs, drawn as alpha masks and tinted by text color.
// The search mark is inline: the asset folder has no search glyph, and the web app inlines it too.

use gpui::{px, size, svg, Hsla, IntoElement, Pixels, Styled, Svg, Transformation};

pub fn icon(data: &'static [u8], color: impl Into<Hsla>, box_size: Pixels) -> impl IntoElement {
    let (width, height) = fit_size(data, box_size);
    svg()
        .data(data)
        .text_color(color)
        .w(width)
        .h(height)
        .flex_shrink_0()
}

// Not inside a generic function. A byte string there becomes an anonymous LLVM
// global, and incremental MSVC then fails the link with LNK2019 (the global is
// referenced from `icon_scaled` and never defined). `fill` stays the word
// `white`: a `#` would end this raw string.
const DISC: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><circle cx="16" cy="16" r="16" fill="white"/></svg>"#;

/// Same box as [`icon`]. `scale` is a paint transform around the glyph center,
/// so the layout size stays on whole pixels while the drawing shrinks.
pub fn icon_scaled(
    data: &'static [u8],
    color: impl Into<Hsla>,
    box_size: Pixels,
    scale: f32,
) -> Svg {
    scaled_svg(data, color.into(), box_size, scale)
}

/// Filled disc used as a button wash. Scaled in paint, like [`icon_scaled`].
pub fn scaled_disc(color: impl Into<Hsla>, box_size: Pixels, scale: f32) -> Svg {
    scaled_svg(DISC, color.into(), box_size, scale)
}

/// One non-generic body. Keeping this out of the generic wrappers stops the
/// scaled SVG from being monomorphized into several codegen units.
#[inline(never)]
fn scaled_svg(data: &'static [u8], color: Hsla, box_size: Pixels, scale: f32) -> Svg {
    // The element box stays the requested square. The pixmap keeps the viewBox
    // aspect and is centered inside it, so a 49×48 glyph and a 49×49 glyph
    // occupy the same slot.
    svg()
        .data(data)
        .text_color(color)
        .w(box_size)
        .h(box_size)
        .flex_shrink_0()
        .with_transformation(Transformation::scale(size(scale, scale)))
}

fn fit_size(data: &[u8], box_size: Pixels) -> (Pixels, Pixels) {
    let (view_w, view_h) = view_box(data);
    let scale = (box_size / px(view_w)).min(box_size / px(view_h));
    (px(view_w * scale), px(view_h * scale))
}

fn view_box(data: &[u8]) -> (f32, f32) {
    let Ok(text) = std::str::from_utf8(data) else {
        return (1.0, 1.0);
    };
    let Some(start) = text.find("viewBox=\"") else {
        return (1.0, 1.0);
    };
    let rest = &text[start + "viewBox=\"".len()..];
    let Some(end) = rest.find('"') else {
        return (1.0, 1.0);
    };
    let mut parts = rest[..end]
        .split_whitespace()
        .filter_map(|part| part.parse::<f32>().ok());
    let _min_x = parts.next();
    let _min_y = parts.next();
    match (parts.next(), parts.next()) {
        (Some(width), Some(height)) if width > 0.0 && height > 0.0 => (width, height),
        _ => (1.0, 1.0),
    }
}

pub const PLAY: &[u8] = include_bytes!("src/icons/svgs/play.svg");
pub const PAUSE: &[u8] = include_bytes!("src/icons/svgs/pause.svg");
pub const PREV: &[u8] = include_bytes!("src/icons/svgs/playbackward.svg");
pub const NEXT: &[u8] = include_bytes!("src/icons/svgs/playforward.svg");
pub const SHUFFLE: &[u8] = include_bytes!("src/icons/svgs/shuffle.svg");
pub const SHUFFLE_OFF: &[u8] = include_bytes!("src/icons/svgs/noshuffle.svg");
pub const REPEAT_ONE: &[u8] = include_bytes!("src/icons/svgs/repeat.svg");
pub const REPEAT_ALL: &[u8] = include_bytes!("src/icons/svgs/repeatplaylist.svg");
pub const REPEAT_OFF: &[u8] = include_bytes!("src/icons/svgs/norepeat.svg");
pub const HEART: &[u8] = include_bytes!("src/icons/svgs/heart.svg");
pub const HEART_FILLED: &[u8] = include_bytes!("src/icons/svgs/heartfilled.svg");
pub const OPTIONS: &[u8] = include_bytes!("src/icons/svgs/options.svg");
pub const SEARCH: &[u8] = include_bytes!("src/icons/svgs/search.svg");
pub const TIME: &[u8] = include_bytes!("src/icons/svgs/time.svg");
pub const MINIMIZE: &[u8] = include_bytes!("src/icons/svgs/minimize.svg");
pub const MAXIMIZE: &[u8] = include_bytes!("src/icons/svgs/maximize.svg");
pub const RESTORE: &[u8] = include_bytes!("src/icons/svgs/revertmaximize.svg");
pub const CLOSE: &[u8] = include_bytes!("src/icons/svgs/close.svg");
pub const PLUS: &[u8] = include_bytes!("src/icons/svgs/plus.svg");
pub const LIST: &[u8] = include_bytes!("src/icons/svgs/text.svg");
pub const VOL_MIN: &[u8] = include_bytes!("src/icons/svgs/volmin.svg");
pub const VOL_MED: &[u8] = include_bytes!("src/icons/svgs/volmed.svg");
pub const VOL_MAX: &[u8] = include_bytes!("src/icons/svgs/volmax.svg");
pub const MUTE: &[u8] = include_bytes!("src/icons/svgs/mute.svg");
