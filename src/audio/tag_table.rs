pub const ARTIST_SEPARATOR: &str = ", ";

pub fn split_tag_multi_values(text: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for chunk in text.split(['\0', ';']) {
        let mut rest = chunk;
        loop {
            let cut = rest.char_indices().find(|(i, ch)| {
                if *ch != '/' {
                    return false;
                }
                let before_ws = rest[..*i]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace);
                let after_ws = rest[i + ch.len_utf8()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace);
                before_ws || after_ws
            });
            match cut {
                Some((i, ch)) => {
                    out.push(&rest[..i]);
                    rest = &rest[i + ch.len_utf8()..];
                }
                None => {
                    out.push(rest);
                    break;
                }
            }
        }
    }
    out.into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect()
}

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::read_from_path;
use lofty::tag::{ItemKey, ItemValue, Tag, TagItem, TagType};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagTableRow {
    pub id: String,
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub read_only: bool,
}

const CORE_FIELDS: &[(ItemKey, &str)] = &[
    (ItemKey::TrackArtist, "Artist Name"),
    (ItemKey::TrackTitle, "Track Title"),
    (ItemKey::AlbumTitle, "Album Title"),
    (ItemKey::RecordingDate, "Date"),
    (ItemKey::Genre, "Genre"),
    (ItemKey::Composer, "Composer"),
    (ItemKey::Performer, "Performer"),
    (ItemKey::AlbumArtist, "Album Artist"),
    (ItemKey::TrackNumber, "Track Number"),
    (ItemKey::TrackTotal, "Total Tracks"),
    (ItemKey::DiscNumber, "Disc Number"),
    (ItemKey::DiscTotal, "Total Discs"),
    (ItemKey::Comment, "Comment"),
];

const EXTRA_LABELS: &[(ItemKey, &str)] = &[
    (ItemKey::Year, "Year"),
    (ItemKey::CopyrightMessage, "<COPYRIGHT>"),
    (ItemKey::License, "<LICENSE>"),
    (ItemKey::ParentalAdvisory, "<ITUNESADVISORY>"),
    (ItemKey::OriginalMediaType, "<ITUNESMEDIATYPE>"),
    (ItemKey::UnsyncLyrics, "<LYRICS>"),
    (ItemKey::Lyrics, "Lyrics"),
    (ItemKey::Conductor, "Conductor"),
    (ItemKey::Lyricist, "Lyricist"),
    (ItemKey::Producer, "Producer"),
    (ItemKey::Remixer, "Remixer"),
    (ItemKey::Publisher, "Publisher"),
    (ItemKey::Label, "Label"),
    (ItemKey::Isrc, "ISRC"),
    (ItemKey::Barcode, "Barcode"),
    (ItemKey::CatalogNumber, "Catalog Number"),
    (ItemKey::EncodedBy, "Encoded By"),
    (ItemKey::EncoderSoftware, "Encoder"),
    (ItemKey::EncoderSettings, "Encoder Settings"),
    (ItemKey::OriginalArtist, "Original Artist"),
    (ItemKey::OriginalAlbumTitle, "Original Album"),
    (ItemKey::OriginalReleaseDate, "Original Release"),
    (ItemKey::ReleaseDate, "Release Date"),
    (ItemKey::Language, "Language"),
    (ItemKey::InitialKey, "Initial Key"),
    (ItemKey::Bpm, "BPM"),
    (ItemKey::IntegerBpm, "BPM (int)"),
    (ItemKey::Mood, "Mood"),
    (ItemKey::ContentGroup, "Content Group"),
    (ItemKey::TrackSubtitle, "Subtitle"),
    (ItemKey::SetSubtitle, "Set Subtitle"),
    (ItemKey::Work, "Work"),
    (ItemKey::Movement, "Movement"),
    (ItemKey::MovementNumber, "Movement Number"),
    (ItemKey::MovementTotal, "Movement Total"),
    (ItemKey::FlagCompilation, "Compilation"),
    (ItemKey::MusicBrainzRecordingId, "MusicBrainz Recording Id"),
    (ItemKey::MusicBrainzTrackId, "MusicBrainz Track Id"),
    (ItemKey::MusicBrainzReleaseId, "MusicBrainz Release Id"),
    (
        ItemKey::MusicBrainzReleaseGroupId,
        "MusicBrainz Release Group Id",
    ),
    (ItemKey::MusicBrainzArtistId, "MusicBrainz Artist Id"),
    (
        ItemKey::MusicBrainzReleaseArtistId,
        "MusicBrainz Album Artist Id",
    ),
    (ItemKey::MusicBrainzWorkId, "MusicBrainz Work Id"),
    (ItemKey::MusicBrainzReleaseType, "MusicBrainz Release Type"),
    (ItemKey::AcoustId, "AcoustID"),
    (ItemKey::AcoustIdFingerprint, "AcoustID Fingerprint"),
    (ItemKey::ReplayGainTrackGain, "ReplayGain Track Gain"),
    (ItemKey::ReplayGainTrackPeak, "ReplayGain Track Peak"),
    (ItemKey::ReplayGainAlbumGain, "ReplayGain Album Gain"),
    (ItemKey::ReplayGainAlbumPeak, "ReplayGain Album Peak"),
    (ItemKey::TrackArtistSortOrder, "Artist Sort"),
    (ItemKey::AlbumArtistSortOrder, "Album Artist Sort"),
    (ItemKey::TrackTitleSortOrder, "Title Sort"),
    (ItemKey::AlbumTitleSortOrder, "Album Sort"),
    (ItemKey::ComposerSortOrder, "Composer Sort"),
];

fn item_key_id(key: ItemKey) -> String {
    format!("{key:?}")
}

fn item_key_label(key: ItemKey) -> String {
    for (k, name) in CORE_FIELDS.iter().chain(EXTRA_LABELS.iter()) {
        if *k == key {
            return (*name).to_string();
        }
    }
    let raw = format!("{key:?}");
    let mut out = String::with_capacity(raw.len() + 4);
    for (i, ch) in raw.chars().enumerate() {
        if i > 0 && ch.is_uppercase() {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

fn parse_item_key(id: &str) -> Option<ItemKey> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    for (key, _) in CORE_FIELDS.iter().chain(EXTRA_LABELS.iter()) {
        if format!("{key:?}") == id {
            return Some(*key);
        }
    }
    match id {
        "TrackArtists" => Some(ItemKey::TrackArtists),
        "AlbumArtists" => Some(ItemKey::AlbumArtists),
        "Arranger" => Some(ItemKey::Arranger),
        "Writer" => Some(ItemKey::Writer),
        "Director" => Some(ItemKey::Director),
        "Engineer" => Some(ItemKey::Engineer),
        "MixDj" => Some(ItemKey::MixDj),
        "MixEngineer" => Some(ItemKey::MixEngineer),
        "Description" => Some(ItemKey::Description),
        "Script" => Some(ItemKey::Script),
        "Color" => Some(ItemKey::Color),
        "FileOwner" => Some(ItemKey::FileOwner),
        "TaggingTime" => Some(ItemKey::TaggingTime),
        "ReleaseCountry" => Some(ItemKey::ReleaseCountry),
        "Popularimeter" => Some(ItemKey::Popularimeter),
        "FlagPodcast" => Some(ItemKey::FlagPodcast),
        "ShowName" => Some(ItemKey::ShowName),
        "InternetRadioStationName" => Some(ItemKey::InternetRadioStationName),
        "InternetRadioStationOwner" => Some(ItemKey::InternetRadioStationOwner),
        "AppleXid" => Some(ItemKey::AppleXid),
        "AppleId3v2ContentGroup" => Some(ItemKey::AppleId3v2ContentGroup),
        _ => None,
    }
}

fn open_tagged(path: &Path) -> Result<lofty::file::TaggedFile, String> {
    read_from_path(path)
        .or_else(|_| {
            Probe::open(path).and_then(|probe| {
                probe
                    .options(ParseOptions::new().read_cover_art(true))
                    .read()
            })
        })
        .map_err(|e| format!("Failed to open audio tags: {e}"))
}

fn open_tagged_text_only(path: &Path) -> Result<lofty::file::TaggedFile, String> {
    Probe::open(path)
        .and_then(|probe| {
            probe
                .options(ParseOptions::new().read_cover_art(false))
                .read()
        })
        .or_else(|_| read_from_path(path))
        .map_err(|e| format!("Failed to open audio tags: {e}"))
}

fn primary_or_insert(tagged: &mut lofty::file::TaggedFile) -> Result<&mut Tag, String> {
    if tagged.primary_tag_mut().is_none() {
        let tag_type = tagged
            .primary_tag()
            .or_else(|| tagged.first_tag())
            .map(|t| t.tag_type())
            .unwrap_or(TagType::Id3v2);
        tagged.insert_tag(Tag::new(tag_type));
    }
    tagged
        .primary_tag_mut()
        .ok_or_else(|| "No writable tag slot".to_string())
}

fn separator_for(key: ItemKey) -> &'static str {
    match key {
        ItemKey::TrackArtist
        | ItemKey::TrackArtists
        | ItemKey::AlbumArtist
        | ItemKey::AlbumArtists
        | ItemKey::Performer
        | ItemKey::OriginalArtist => ARTIST_SEPARATOR,
        _ => "; ",
    }
}

pub fn read_embedded_lyrics(path: &Path) -> Result<Option<String>, String> {
    let tagged = open_tagged_text_only(path)?;

    let primary_type = tagged.primary_tag().map(|tag| tag.tag_type());
    let tags = tagged.primary_tag().into_iter().chain(
        tagged
            .tags()
            .iter()
            .filter(|tag| Some(tag.tag_type()) != primary_type),
    );

    for tag in tags {
        for key in [ItemKey::UnsyncLyrics, ItemKey::Lyrics] {
            for item in tag.items().filter(|item| item.key() == key) {
                let text = match item.value() {
                    ItemValue::Text(s) | ItemValue::Locator(s) => s,
                    ItemValue::Binary(_) => continue,
                };
                if !text.trim().is_empty() {
                    return Ok(Some(text.to_string()));
                }
            }
        }
    }

    Ok(None)
}

pub fn read_tag_table(path: &Path) -> Result<Vec<TagTableRow>, String> {
    let tagged = open_tagged_text_only(path)?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());

    let mut values: HashMap<ItemKey, Vec<String>> = HashMap::new();
    if let Some(tag) = tag {
        for item in tag.items() {
            let text = match item.value() {
                ItemValue::Text(s) | ItemValue::Locator(s) => {
                    let t = s.trim();
                    if t.is_empty() {
                        continue;
                    }
                    t
                }
                ItemValue::Binary(_) => continue,
            };
            let key = item.key();
            let entry = values.entry(key).or_default();
            if separator_for(key) == ARTIST_SEPARATOR {
                for part in split_tag_multi_values(text) {
                    if entry.iter().any(|seen| seen.eq_ignore_ascii_case(part)) {
                        continue;
                    }
                    entry.push(part.to_string());
                }
            } else {
                entry.push(text.to_string());
            }
        }
    }

    let mut seen: HashSet<ItemKey> = HashSet::new();
    let mut rows: Vec<TagTableRow> = Vec::new();

    for (key, name) in CORE_FIELDS {
        seen.insert(*key);
        let value = values
            .get(key)
            .map(|v| v.join(separator_for(*key)))
            .unwrap_or_default();
        rows.push(TagTableRow {
            id: item_key_id(*key),
            name: (*name).to_string(),
            value,
            read_only: false,
        });
    }
    let mut extras: Vec<(ItemKey, String)> = values
        .into_iter()
        .filter(|(k, v)| !seen.contains(k) && v.iter().any(|s| !s.trim().is_empty()))
        .map(|(k, v)| {
            let sep = separator_for(k);
            (k, v.join(sep))
        })
        .collect();
    extras.sort_by_key(|a| item_key_label(a.0));

    for (key, value) in extras {
        rows.push(TagTableRow {
            id: item_key_id(key),
            name: item_key_label(key),
            value,
            read_only: false,
        });
    }

    Ok(rows)
}

/// Write the table back. Empty values remove the field; non-empty set it.
pub fn write_tag_table(path: &Path, rows: &[TagTableRow]) -> Result<(), String> {
    let mut tagged = open_tagged(path)?;
    let tag = primary_or_insert(&mut tagged)?;

    let mut desired: HashMap<ItemKey, String> = HashMap::new();
    for row in rows {
        if row.read_only {
            continue;
        }
        let Some(key) = parse_item_key(&row.id) else {
            continue;
        };
        let value = row.value.trim();
        if value.is_empty() {
            desired.insert(key, String::new());
        } else {
            desired.insert(key, value.to_string());
        }
    }

    for (key, value) in &desired {
        if value.is_empty() {
            tag.remove_key(*key);
            continue;
        }

        if separator_for(*key) == ARTIST_SEPARATOR {
            let parts: Vec<&str> = value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            tag.remove_key(*key);
            for part in parts {
                tag.push(TagItem::new(*key, ItemValue::Text(part.to_string())));
            }
            continue;
        }

        tag.insert_text(*key, value.clone());
    }

    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("Failed to save tags: {e}"))?;
    Ok(())
}

pub fn write_bpm(path: &Path, bpm: f32) -> Result<(), String> {
    if !bpm.is_finite() || bpm <= 0.0 {
        return Err("Invalid BPM".into());
    }
    let bpm = bpm.clamp(1.0, 999.0);
    let int_bpm = bpm.round().clamp(1.0, 999.0) as u32;
    let text = if (bpm - int_bpm as f32).abs() < 0.05 {
        format!("{int_bpm}")
    } else {
        format!("{bpm:.1}")
    };

    let mut tagged = open_tagged(path)?;
    let tag = primary_or_insert(&mut tagged)?;
    tag.insert_text(ItemKey::Bpm, text.clone());
    tag.insert_text(ItemKey::IntegerBpm, int_bpm.to_string());
    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("Failed to save BPM tags: {e}"))?;
    Ok(())
}

pub fn read_bpm(path: &Path) -> Option<f32> {
    let tagged = open_tagged_text_only(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    let parse = |s: &str| -> Option<f32> {
        let t = s.trim().replace(',', ".");
        let v: f32 = t.parse().ok()?;
        if v.is_finite() && v > 0.0 && v < 1000.0 {
            Some(v)
        } else {
            None
        }
    };
    tag.get_string(ItemKey::Bpm)
        .and_then(parse)
        .or_else(|| tag.get_string(ItemKey::IntegerBpm).and_then(parse))
}

pub fn tag_table_from_core_fields(
    title: Option<&str>,
    artist: Option<&str>,
    album: Option<&str>,
    genre: Option<&str>,
    year: Option<u32>,
    track_number: Option<u32>,
) -> Vec<TagTableRow> {
    let mut values: HashMap<ItemKey, String> = HashMap::new();
    let put = |map: &mut HashMap<ItemKey, String>, key: ItemKey, value: Option<&str>| {
        if let Some(s) = value.map(str::trim).filter(|s| !s.is_empty()) {
            map.insert(key, s.to_string());
        }
    };
    put(&mut values, ItemKey::TrackTitle, title);
    put(&mut values, ItemKey::TrackArtist, artist);
    put(&mut values, ItemKey::AlbumTitle, album);
    put(&mut values, ItemKey::Genre, genre);
    if let Some(y) = year.filter(|&y| y > 0) {
        values.insert(ItemKey::RecordingDate, y.to_string());
    }
    if let Some(n) = track_number.filter(|&n| n > 0) {
        values.insert(ItemKey::TrackNumber, n.to_string());
    }

    CORE_FIELDS
        .iter()
        .map(|(key, name)| TagTableRow {
            id: item_key_id(*key),
            name: (*name).to_string(),
            value: values.get(key).cloned().unwrap_or_default(),
            read_only: false,
        })
        .collect()
}

pub fn track_fields_from_table(rows: &[TagTableRow]) -> TrackFieldsFromTable {
    let get = |id: &str| -> Option<String> {
        rows.iter()
            .find(|r| r.id == id)
            .map(|r| r.value.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let year = get("Year").or_else(|| get("RecordingDate")).and_then(|s| {
        let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok().filter(|&y: &u32| y > 0 && y <= 9999)
    });
    let track_number = get("TrackNumber").and_then(|s| {
        s.split(['/', '-'])
            .next()
            .and_then(|p| p.trim().parse().ok())
            .filter(|&n: &u32| n > 0)
    });

    TrackFieldsFromTable {
        title: get("TrackTitle"),
        artist: get("TrackArtist"),
        album: get("AlbumTitle"),
        genre: get("Genre"),
        year,
        track_number,
    }
}

#[derive(Debug, Clone, Default)]
pub struct TrackFieldsFromTable {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip_core_ids() {
        for (key, _) in CORE_FIELDS {
            let id = item_key_id(*key);
            assert_eq!(parse_item_key(&id), Some(*key), "id={id}");
        }
    }

    #[test]
    fn artist_split_keeps_acdc_and_splits_spaced_slash() {
        let parts = split_tag_multi_values("AC/DC; Sammy Virji / Skream\0Extra");
        assert_eq!(parts, vec!["AC/DC", "Sammy Virji", "Skream", "Extra"]);
    }
}
