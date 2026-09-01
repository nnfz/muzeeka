// Foobar-style full tag table: dump / edit every text field lofty can map.

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::read_from_path;
use lofty::tag::{ItemKey, ItemValue, Tag, TagType};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// One row in the properties metadata table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagTableRow {
    /// Stable internal id (ItemKey Debug name or freeform id).
    pub id: String,
    /// Foobar-style display name.
    pub name: String,
    pub value: String,
    /// Binary / non-editable payload.
    #[serde(default)]
    pub read_only: bool,
}

/// Core rows always shown (even empty) — foobar's main columns.
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

/// Friendly names for non-core keys (only listed when present in the file).
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
    (ItemKey::MusicBrainzReleaseGroupId, "MusicBrainz Release Group Id"),
    (ItemKey::MusicBrainzArtistId, "MusicBrainz Artist Id"),
    (ItemKey::MusicBrainzReleaseArtistId, "MusicBrainz Album Artist Id"),
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
    // Fallback: Debug name with spaces (TrackArtist -> Track Artist)
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
    // Keys that may appear on disk but aren't in the label tables above.
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
    // Always load pictures when opening for write so save doesn't strip APIC/cover.
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

/// Read every text-ish tag into a foobar-ordered table.
pub fn read_tag_table(path: &Path) -> Result<Vec<TagTableRow>, String> {
    let tagged = open_tagged_text_only(path)?;
    let tag = tagged
        .primary_tag()
        .or_else(|| tagged.first_tag());

    let mut values: HashMap<ItemKey, Vec<String>> = HashMap::new();
    if let Some(tag) = tag {
        for item in tag.items() {
            let text = match item.value() {
                ItemValue::Text(s) | ItemValue::Locator(s) => {
                    let t = s.trim();
                    if t.is_empty() {
                        continue;
                    }
                    t.to_string()
                }
                ItemValue::Binary(_) => continue,
            };
            values.entry(item.key()).or_default().push(text);
        }
    }

    let mut seen: HashSet<ItemKey> = HashSet::new();
    let mut rows: Vec<TagTableRow> = Vec::new();

    // 1) Core foobar columns — always listed (empty = clearable).
    for (key, name) in CORE_FIELDS {
        seen.insert(*key);
        let value = values
            .get(key)
            .map(|v| v.join("; "))
            .unwrap_or_default();
        rows.push(TagTableRow {
            id: item_key_id(*key),
            name: (*name).to_string(),
            value,
            read_only: false,
        });
    }

    // 2) Everything else only if the file actually has it (no empty junk rows).
    //    Includes embedded lyrics when present — same as foobar's tag dump.
    let mut extras: Vec<(ItemKey, String)> = values
        .into_iter()
        .filter(|(k, v)| !seen.contains(k) && v.iter().any(|s| !s.trim().is_empty()))
        .map(|(k, v)| (k, v.join("; ")))
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
/// Pictures and unlisted binary frames are preserved.
pub fn write_tag_table(path: &Path, rows: &[TagTableRow]) -> Result<(), String> {
    let mut tagged = open_tagged(path)?;
    let tag = primary_or_insert(&mut tagged)?;

    // Collect desired text state.
    let mut desired: HashMap<ItemKey, String> = HashMap::new();
    for row in rows {
        if row.read_only {
            continue;
        }
        let Some(key) = parse_item_key(&row.id) else {
            // Unknown freeform ids — skip for now (lofty has no generic Unknown key).
            continue;
        };
        let value = row.value.trim();
        if value.is_empty() {
            desired.insert(key, String::new());
        } else {
            desired.insert(key, value.to_string());
        }
    }

    // Apply: remove empties, set non-empties.
    for (key, value) in &desired {
        if value.is_empty() {
            tag.remove_key(*key);
        } else {
            // Multi-value joined with "; " becomes a single text field (foobar-like).
            tag.insert_text(*key, value.clone());
        }
    }

    // Also remove keys that existed and were in standard list but missing from payload
    // (frontend always sends full table, so desired covers all editable rows).

    tagged
        .save_to_path(path, WriteOptions::default())
        .map_err(|e| format!("Failed to save tags: {e}"))?;
    Ok(())
}

/// Write BPM into file tags (`Bpm` + `IntegerBpm` for max format compatibility).
/// Does not touch other fields.
pub fn write_bpm(path: &Path, bpm: f32) -> Result<(), String> {
    if !bpm.is_finite() || bpm <= 0.0 {
        return Err("Invalid BPM".into());
    }
    let bpm = bpm.clamp(1.0, 999.0);
    let int_bpm = bpm.round().clamp(1.0, 999.0) as u32;
    // Prefer a clean integer string when close; keep one decimal otherwise.
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

/// Read BPM from tags (`Bpm` preferred, then `IntegerBpm`).
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

/// Core foobar columns filled from a library row (radio streams have no on-disk tags).
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

/// Pull common library fields out of a saved table for SQLite / UI.
pub fn track_fields_from_table(rows: &[TagTableRow]) -> TrackFieldsFromTable {
    let get = |id: &str| -> Option<String> {
        rows.iter()
            .find(|r| r.id == id)
            .map(|r| r.value.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let year = get("Year")
        .or_else(|| get("RecordingDate"))
        .and_then(|s| {
            // "2020" or "2020-01-01"
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
}
