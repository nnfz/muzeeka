// Accent sampled from the playing track's cover.
//
// The chrome is near-black, so the chosen color is pushed into a narrow
// lightness band. Hue still comes from the picture. A gray or empty cover
// leaves the caller on the default purple.

use std::path::Path;

const HUE_BINS: usize = 16;
const LIGHT_BINS: usize = 3;
const BINS: usize = HUE_BINS * LIGHT_BINS;

/// RGB hex (`0xRRGGBB`) for `path`, or `None` when the file is not a picture
/// or has no color that can sit on the dark UI.
pub fn from_cover(path: &Path) -> Option<u32> {
    let image = image::open(path).ok()?;
    let thumb = image.thumbnail(32, 32).into_rgba8();
    from_rgba(thumb.as_raw())
}

pub(crate) fn from_rgba(bytes: &[u8]) -> Option<u32> {
    let mut count = [0u32; BINS];
    let mut saturation = [0f32; BINS];
    let mut red = [0f32; BINS];
    let mut green = [0f32; BINS];
    let mut blue = [0f32; BINS];
    let mut lightness = [0f32; BINS];

    for pixel in bytes.chunks_exact(4) {
        if pixel[3] < 128 {
            continue;
        }
        let (hue, sat, light) = rgb_to_hsl(pixel[0], pixel[1], pixel[2]);
        // JPEG noise on black and white is slightly tinted. It is not a color.
        if sat < 0.18 || !(0.08..0.94).contains(&light) {
            continue;
        }
        let hue_bin = ((hue * HUE_BINS as f32) as usize).min(HUE_BINS - 1);
        let light_bin = if light < 0.35 {
            0
        } else if light < 0.70 {
            1
        } else {
            2
        };
        let index = hue_bin * LIGHT_BINS + light_bin;
        count[index] += 1;
        saturation[index] += sat;
        red[index] += pixel[0] as f32;
        green[index] += pixel[1] as f32;
        blue[index] += pixel[2] as f32;
        lightness[index] += light;
    }

    let mut best: Option<(usize, f32)> = None;
    for index in 0..BINS {
        let n = count[index];
        if n == 0 {
            continue;
        }
        let average_sat = saturation[index] / n as f32;
        // A small vivid logo beats a large dull wash. A large vivid area still wins.
        let score = (n as f32).sqrt() * average_sat * average_sat;
        let replace = match best {
            Some((_, previous)) => score > previous,
            None => true,
        };
        if replace {
            best = Some((index, score));
        }
    }
    let (index, _) = best?;
    let n = count[index] as f32;
    let (hue, sat, _) = rgb_to_hsl(
        (red[index] / n).round() as u8,
        (green[index] / n).round() as u8,
        (blue[index] / n).round() as u8,
    );
    let light = (lightness[index] / n).clamp(0.48, 0.64);
    let (r, g, b) = hsl_to_rgb(hue, sat.clamp(0.42, 0.88), light);
    Some(((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
}

fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let light = (max + min) * 0.5;
    let delta = max - min;
    if delta < 1e-6 {
        return (0.0, 0.0, light);
    }
    let sat = if light > 0.5 {
        delta / (2.0 - max - min)
    } else {
        delta / (max + min)
    };
    let hue = if max == r {
        (g - b) / delta + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } / 6.0;
    (hue, sat, light)
}

fn hsl_to_rgb(hue: f32, sat: f32, light: f32) -> (u8, u8, u8) {
    let sat = sat.clamp(0.0, 1.0);
    let light = light.clamp(0.0, 1.0);
    if sat <= f32::EPSILON {
        let value = (light * 255.0).round() as u8;
        return (value, value, value);
    }
    let q = if light < 0.5 {
        light * (1.0 + sat)
    } else {
        light + sat - light * sat
    };
    let p = 2.0 * light - q;
    let channel = |offset: f32| {
        let t = (hue + offset).rem_euclid(1.0);
        let value = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (value * 255.0).round() as u8
    };
    (channel(1.0 / 3.0), channel(0.0), channel(-1.0 / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(pixel: [u8; 4]) -> Vec<u8> {
        image::RgbaImage::from_pixel(8, 8, image::Rgba(pixel)).into_raw()
    }

    fn channels(rgb: u32) -> (u32, u32, u32) {
        ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff)
    }

    #[test]
    fn red_cover_stays_red() {
        let (r, g, b) = channels(from_rgba(&solid([220, 20, 30, 255])).unwrap());
        assert!(r > g + 40 && r > b + 40, "rgb {r} {g} {b}");
    }

    #[test]
    fn gray_and_black_have_no_accent() {
        assert!(from_rgba(&solid([40, 40, 40, 255])).is_none());
        assert!(from_rgba(&solid([0, 0, 0, 255])).is_none());
        assert!(from_rgba(&solid([250, 250, 250, 255])).is_none());
    }

    #[test]
    fn small_vivid_patch_beats_black() {
        let mut image = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 0, 255]));
        for x in 0..3 {
            image.put_pixel(x, 0, image::Rgba([20, 40, 230, 255]));
        }
        let (r, g, b) = channels(from_rgba(image.as_raw()).unwrap());
        assert!(b > r + 40 && b > g + 40, "rgb {r} {g} {b}");
    }

    #[test]
    fn missing_cover_file_has_no_accent() {
        assert!(from_cover(Path::new("Z:/no/such/cover.webp")).is_none());
    }
}
