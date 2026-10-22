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

const DISC: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><circle cx="16" cy="16" r="16" fill="white"/></svg>"#;

pub fn icon_scaled(
    data: &'static [u8],
    color: impl Into<Hsla>,
    box_size: Pixels,
    scale: f32,
) -> Svg {
    scaled_svg(data, color.into(), box_size, scale)
}

pub fn scaled_disc(color: impl Into<Hsla>, box_size: Pixels, scale: f32) -> Svg {
    scaled_svg(DISC, color.into(), box_size, scale)
}

#[inline(never)]
fn scaled_svg(data: &'static [u8], color: Hsla, box_size: Pixels, scale: f32) -> Svg {
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

// player
pub const PLAY: &[u8] = include_bytes!("src/icons/svgs/play.svg");
pub const PAUSE: &[u8] = include_bytes!("src/icons/svgs/pause.svg");
pub const PREV: &[u8] = include_bytes!("src/icons/svgs/playbackward.svg");
pub const NEXT: &[u8] = include_bytes!("src/icons/svgs/playforward.svg");
pub const SHUFFLE: &[u8] = include_bytes!("src/icons/svgs/shuffle.svg");
pub const SHUFFLE_OFF: &[u8] = include_bytes!("src/icons/svgs/noshuffle.svg");
/// Die. The pips are holes, so the tint shows the face and not five dots.
pub const RANDOM: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 49 49"><path fill="white" fill-rule="evenodd" d="M14 6h21a8 8 0 0 1 8 8v21a8 8 0 0 1-8 8H14a8 8 0 0 1-8-8V14a8 8 0 0 1 8-8zm2 7a3 3 0 1 0 6 0a3 3 0 1 0-6 0zm13 0a3 3 0 1 0 6 0a3 3 0 1 0-6 0zm-6.5 6.5a3 3 0 1 0 6 0a3 3 0 1 0-6 0zm-6.5 6.5a3 3 0 1 0 6 0a3 3 0 1 0-6 0zm13 0a3 3 0 1 0 6 0a3 3 0 1 0-6 0z"/></svg>"#;
pub const REPEAT_ONE: &[u8] = include_bytes!("src/icons/svgs/repeat.svg");
pub const REPEAT_ALL: &[u8] = include_bytes!("src/icons/svgs/repeatplaylist.svg");
pub const REPEAT_OFF: &[u8] = include_bytes!("src/icons/svgs/norepeat.svg");
pub const VOL_MIN: &[u8] = include_bytes!("src/icons/svgs/volmin.svg");
pub const VOL_MED: &[u8] = include_bytes!("src/icons/svgs/volmed.svg");
pub const VOL_MAX: &[u8] = include_bytes!("src/icons/svgs/volmax.svg");
pub const MUTE: &[u8] = include_bytes!("src/icons/svgs/mute.svg");

// track
pub const HEART: &[u8] = include_bytes!("src/icons/svgs/heart.svg");
pub const HEART_FILLED: &[u8] = include_bytes!("src/icons/svgs/heartfilled.svg");

// main
pub const OPTIONS: &[u8] = include_bytes!("src/icons/svgs/options.svg");
pub const SEARCH: &[u8] = include_bytes!("src/icons/svgs/search.svg");
pub const MINIMIZE: &[u8] = include_bytes!("src/icons/svgs/minimize.svg");
pub const MAXIMIZE: &[u8] = include_bytes!("src/icons/svgs/maximize.svg");
pub const RESTORE: &[u8] = include_bytes!("src/icons/svgs/revertmaximize.svg");
pub const CLOSE: &[u8] = include_bytes!("src/icons/svgs/close.svg");

// playlist
pub const TIME: &[u8] = include_bytes!("src/icons/svgs/time.svg");
pub const PLUS: &[u8] = include_bytes!("src/icons/svgs/plus.svg");
pub const ALL: &[u8] = include_bytes!("src/icons/svgs/all.svg");
