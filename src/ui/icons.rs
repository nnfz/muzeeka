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

pub const PLAY: &[u8] = include_bytes!("src/icons/svgs/play.svg");
pub const PAUSE: &[u8] = include_bytes!("src/icons/svgs/pause.svg");
pub const PREV: &[u8] = include_bytes!("src/icons/svgs/playbackward.svg");
pub const NEXT: &[u8] = include_bytes!("src/icons/svgs/playforward.svg");
pub const SHUFFLE: &[u8] = include_bytes!("src/icons/svgs/shuffle.svg");
pub const SHUFFLE_OFF: &[u8] = include_bytes!("src/icons/svgs/noshuffle.svg");
pub const RANDOM: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 49 49"><path fill="white" fill-rule="evenodd" d="M14 6h21a8 8 0 0 1 8 8v21a8 8 0 0 1-8 8H14a8 8 0 0 1-8-8V14a8 8 0 0 1 8-8zm2 7a3 3 0 1 0 6 0a3 3 0 1 0-6 0zm13 0a3 3 0 1 0 6 0a3 3 0 1 0-6 0zm-6.5 6.5a3 3 0 1 0 6 0a3 3 0 1 0-6 0zm-6.5 6.5a3 3 0 1 0 6 0a3 3 0 1 0-6 0zm13 0a3 3 0 1 0 6 0a3 3 0 1 0-6 0z"/></svg>"#;
pub const REPEAT_ONE: &[u8] = include_bytes!("src/icons/svgs/repeat.svg");
pub const REPEAT_ALL: &[u8] = include_bytes!("src/icons/svgs/repeatplaylist.svg");
pub const REPEAT_OFF: &[u8] = include_bytes!("src/icons/svgs/norepeat.svg");
pub const VOL_MIN: &[u8] = include_bytes!("src/icons/svgs/volmin.svg");
pub const VOL_MED: &[u8] = include_bytes!("src/icons/svgs/volmed.svg");
pub const VOL_MAX: &[u8] = include_bytes!("src/icons/svgs/volmax.svg");
pub const MUTE: &[u8] = include_bytes!("src/icons/svgs/mute.svg");
pub const LYRICS: &[u8] = include_bytes!("src/icons/svgs/text.svg");
pub const LYRICS_OFF: &[u8] = include_bytes!("src/icons/svgs/textclose.svg");
pub const CLOSE_FULLSCREEN: &[u8] = include_bytes!("src/icons/svgs/closefullscreen.svg");

pub const HEART: &[u8] = include_bytes!("src/icons/svgs/heart.svg");
pub const HEART_FILLED: &[u8] = include_bytes!("src/icons/svgs/heartfilled.svg");

pub const OPTIONS: &[u8] = include_bytes!("src/icons/svgs/options.svg");
pub const SEARCH: &[u8] = include_bytes!("src/icons/svgs/search.svg");
pub const MINIMIZE: &[u8] = include_bytes!("src/icons/svgs/minimize.svg");
pub const MAXIMIZE: &[u8] = include_bytes!("src/icons/svgs/maximize.svg");
pub const RESTORE: &[u8] = include_bytes!("src/icons/svgs/revertmaximize.svg");
pub const CLOSE: &[u8] = include_bytes!("src/icons/svgs/close.svg");

pub const TIME: &[u8] = include_bytes!("src/icons/svgs/time.svg");
pub const PLUS: &[u8] = include_bytes!("src/icons/svgs/plus.svg");
pub const ALL: &[u8] = include_bytes!("src/icons/svgs/all.svg");

pub const NOTE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#efefef" d="M9 17.5a2.5 2.5 0 1 1-2-2.45V5.2l12-2v10.15a2.5 2.5 0 1 1-2-2.45V6.1l-8 1.35V17.5z"/></svg>"##;
pub const ARROW: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#efefef" d="M11 3h2v9.2l3.3-3.3 1.4 1.4L12 16.1 6.3 10.3l1.4-1.4L11 12.2V3zM5 19h14v2H5z"/></svg>"##;
pub const SPINNER: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="2.2" r="1.7" fill="#efefef"/><circle cx="12.4" cy="4.6" r="1.45" fill="#efefef" opacity=".8"/><circle cx="13.8" cy="8.8" r="1.2" fill="#efefef" opacity=".55"/><circle cx="11.2" cy="12.4" r="1.05" fill="#efefef" opacity=".35"/><circle cx="7.2" cy="13.6" r="0.9" fill="#efefef" opacity=".2"/></svg>"##;
