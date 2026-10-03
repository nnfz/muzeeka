use std::path::Path;
use std::sync::{Arc, OnceLock};

use gpui::{div, img, prelude::*, px, AnyElement, Image, ImageFormat, IntoElement, ObjectFit};

/// Same 4px as the web `--radius-mini` on track, search, and playlist covers.
/// The radius has to live on the image: a parent's `overflow_hidden` clips to a
/// rectangle, so it does not round the sprite.
const COVER_RADIUS: f32 = 4.;

pub fn cover_art(path: Option<&Path>, size: f32) -> AnyElement {
    let image = match path {
        Some(path) => img(path.to_path_buf()),
        None => img(placeholder_image()),
    };
    // Without aspect_square, Img copies the file's ratio and a wide cover
    // stretches the slot. Cover then crops that square.
    div()
        .size(px(size))
        .min_w(px(size))
        .min_h(px(size))
        .flex_shrink_0()
        .overflow_hidden()
        .child(
            image
                .size(px(size))
                .aspect_square()
                .flex_shrink_0()
                .rounded(px(COVER_RADIUS))
                .object_fit(ObjectFit::Cover),
        )
        .into_any_element()
}

fn placeholder_image() -> Arc<Image> {
    static IMAGE: OnceLock<Arc<Image>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            Arc::new(Image::from_bytes(
                ImageFormat::Webp,
                include_bytes!("src/icons/cover-placeholder.webp").to_vec(),
            ))
        })
        .clone()
}
