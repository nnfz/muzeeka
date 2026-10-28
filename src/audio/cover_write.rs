use std::fs;
use std::path::Path;

use id3::TagLike;
use image::ImageFormat;
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::read_from_path;
use lofty::tag::{Tag, TagType};

fn mime_from_image_bytes(data: &[u8], hint: Option<&str>) -> MimeType {
    if let Some(hint) = hint {
        let hint = hint.to_ascii_lowercase();
        if hint.contains("png") {
            return MimeType::Png;
        }
        if hint.contains("gif") {
            return MimeType::Gif;
        }
        if hint.contains("jpeg") || hint.contains("jpg") {
            return MimeType::Jpeg;
        }
        if hint.contains("webp") {
            return MimeType::Unknown("image/webp".into());
        }
        if hint.contains("image/") {
            return MimeType::from_str(hint.split(';').next().unwrap_or(hint.as_str()).trim());
        }
    }
    if data.len() >= 3 && data[0] == 0xFF && data[1] == 0xD8 && data[2] == 0xFF {
        return MimeType::Jpeg;
    }
    if data.len() >= 8 && &data[0..8] == b"\x89PNG\r\n\x1a\n" {
        return MimeType::Png;
    }
    if data.len() >= 6 && (&data[0..6] == b"GIF87a" || &data[0..6] == b"GIF89a") {
        return MimeType::Gif;
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return MimeType::Unknown("image/webp".into());
    }
    MimeType::Jpeg
}

fn mime_to_id3_str(mime: &MimeType) -> String {
    match mime {
        MimeType::Png => "image/png".to_string(),
        MimeType::Gif => "image/gif".to_string(),
        MimeType::Bmp => "image/bmp".to_string(),
        MimeType::Tiff => "image/tiff".to_string(),
        MimeType::Jpeg => "image/jpeg".to_string(),
        MimeType::Unknown(value) => value.clone(),
        _ => "image/jpeg".to_string(),
    }
}

fn prepare_cover_bytes(
    data: &[u8],
    mime_hint: Option<&str>,
) -> Result<(Vec<u8>, MimeType), String> {
    if data.is_empty() {
        return Err("Empty cover image".to_string());
    }
    match mime_from_image_bytes(data, mime_hint) {
        MimeType::Jpeg | MimeType::Png | MimeType::Gif | MimeType::Bmp | MimeType::Tiff => {
            Ok((data.to_vec(), mime_from_image_bytes(data, mime_hint)))
        }
        _ => match image::load_from_memory(data) {
            Ok(img) => {
                let mut out = Vec::new();
                let rgb = img.to_rgb8();
                let mut cursor = std::io::Cursor::new(&mut out);
                image::DynamicImage::ImageRgb8(rgb)
                    .write_to(&mut cursor, ImageFormat::Jpeg)
                    .map_err(|error| format!("Failed to re-encode cover: {error}"))?;
                Ok((out, MimeType::Jpeg))
            }
            Err(_) => Ok((data.to_vec(), mime_from_image_bytes(data, mime_hint))),
        },
    }
}

fn write_cover_into_file(path: &Path, bytes: &[u8], mime: &MimeType) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());

    if ext.as_deref() == Some("mp3") {
        let mut tag = match id3::Tag::read_from_path(path) {
            Ok(tag) => tag,
            Err(error) => {
                eprintln!(
                    "[cover] id3 read failed for {}: {error} — writing a new tag",
                    path.display()
                );
                id3::Tag::new()
            }
        };
        tag.remove_all_pictures();
        tag.add_frame(id3::frame::Picture {
            mime_type: mime_to_id3_str(mime),
            picture_type: id3::frame::PictureType::CoverFront,
            description: String::new(),
            data: bytes.to_vec(),
        });
        tag.write_to_path(path, id3::Version::Id3v23)
            .map_err(|error| format!("id3 save cover error: {error}"))?;
        return Ok(());
    }

    let mut tagged_file = read_from_path(path)
        .or_else(|_| {
            Probe::open(path).and_then(|probe| {
                probe
                    .options(lofty::config::ParseOptions::new().read_cover_art(true))
                    .read()
            })
        })
        .map_err(|error| format!("Failed to read audio file for cover: {error}"))?;

    if tagged_file.primary_tag_mut().is_none() {
        let tag_type = tagged_file
            .primary_tag()
            .or_else(|| tagged_file.first_tag())
            .map(|tag| tag.tag_type())
            .unwrap_or(TagType::Id3v2);
        tagged_file.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged_file
        .primary_tag_mut()
        .ok_or_else(|| "No writable tag slot".to_string())?;
    tag.remove_picture_type(PictureType::CoverFront);
    tag.remove_picture_type(PictureType::Other);
    let picture = Picture::unchecked(bytes.to_vec())
        .pic_type(PictureType::CoverFront)
        .mime_type(mime.clone())
        .build();
    tag.push_picture(picture);
    tagged_file
        .save_to_path(path, WriteOptions::default())
        .map_err(|error| format!("Failed to save cover tag: {error}"))?;
    Ok(())
}

fn file_has_embedded_cover(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());
    if ext.as_deref() == Some("mp3") {
        if let Ok(tag) = id3::Tag::read_from_path(path) {
            return tag.pictures().next().is_some();
        }
    }
    if let Ok(tagged) = read_from_path(path) {
        if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
            return !tag.pictures().is_empty();
        }
    }
    false
}

fn replace_audio_file(tmp: &Path, dest: &Path) -> Result<(), String> {
    if fs::rename(tmp, dest).is_ok() {
        return Ok(());
    }
    match fs::remove_file(dest) {
        Ok(()) => fs::rename(tmp, dest).map_err(|error| {
            format!("Failed to move tagged file into place ({error}). Stop playback and try again.")
        }),
        Err(error) => Err(format!(
            "Cannot replace audio file ({error}). If the track is playing, stop it and try again."
        )),
    }
}

pub fn write_track_cover(path: &Path, data: &[u8], mime_hint: Option<&str>) -> Result<(), String> {
    let (bytes, mime) = prepare_cover_bytes(data, mime_hint)?;
    match write_cover_into_file(path, &bytes, &mime) {
        Ok(()) if file_has_embedded_cover(path) => return Ok(()),
        Ok(()) => {
            eprintln!(
                "[cover] write reported ok but re-read found no picture: {}",
                path.display()
            );
        }
        Err(error) => {
            eprintln!(
                "[cover] in-place write failed for {}: {error} — trying temp copy",
                path.display()
            );
        }
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio");
    let tmp = path.with_file_name(format!(".{file_name}.muzeeka-cover.tmp"));
    if tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    fs::copy(path, &tmp)
        .map_err(|error| format!("Failed to copy audio for cover write: {error}"))?;
    if let Err(error) = write_cover_into_file(&tmp, &bytes, &mime) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    if !file_has_embedded_cover(&tmp) {
        let _ = fs::remove_file(&tmp);
        return Err("Cover was not embedded (this format may not support picture tags)".into());
    }
    if let Err(error) = replace_audio_file(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    let _ = fs::remove_file(&tmp);
    if !file_has_embedded_cover(path) {
        return Err("Cover write finished but the file still has no embedded picture".into());
    }
    Ok(())
}
