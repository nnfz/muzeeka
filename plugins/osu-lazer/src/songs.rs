//! Beatmap audio extracted from an osu!lazer `client.realm`.

use std::collections::HashMap;

use crate::realm::Realm;

#[derive(Clone, Debug)]
pub struct Song {
    pub hash: String,
    pub audio_filename: String,
    pub artist: String,
    pub title: String,
    /// Empty when the map has no background, or the file is not in the set.
    pub background_hash: String,
    pub background_filename: String,
}

#[derive(Clone, Debug)]
pub struct Survey {
    pub songs: Vec<Song>,
    pub beatmaps: usize,
    pub sets: usize,
}

pub fn survey(bytes: &[u8]) -> Result<Survey, String> {
    let realm = Realm::parse(bytes)?;
    let files = string_map(&realm, "class_File", &["Hash"])?;
    let usages = usage_map(&realm)?;
    let metas = meta_map(&realm)?;
    let set_files = set_file_lists(&realm)?;
    let beatmaps = realm.table("class_Beatmap")?;
    let meta_links = beatmaps.links(&["Metadata"])?;
    let set_links = beatmaps.links(&["BeatmapSet"])?;
    let set_by_beatmap: HashMap<i64, Option<i64>> = set_links.into_iter().collect();

    let mut songs = Vec::new();
    let mut seen = HashMap::<String, usize>::new();
    let beatmap_count = meta_links.len();

    for (beatmap_key, meta_key) in meta_links {
        let Some(meta_key) = meta_key else { continue };
        let Some(meta) = metas.get(&meta_key) else { continue };
        if meta.audio.trim().is_empty() {
            continue;
        }
        let Some(set_key) = set_by_beatmap.get(&beatmap_key).copied().flatten() else {
            continue;
        };
        let Some(usage_keys) = set_files.get(&set_key) else {
            continue;
        };
        let usage = usage_keys.iter().find_map(|key| {
            let usage = usages.get(key)?;
            if names_match(&usage.filename, &meta.audio) {
                Some(usage)
            } else {
                None
            }
        });
        let Some(usage) = usage else { continue };
        let Some(hash) = files.get(&usage.file_key) else {
            continue;
        };
        let hash = normalize_hash(hash);
        if hash.len() != 64 {
            continue;
        }
        let (background_hash, background_filename) =
            background_for(meta, usage_keys, &usages, &files);
        if let Some(index) = seen.get(&hash).copied() {
            prefer_names(&mut songs[index], &meta.artist, &meta.title);
            if songs[index].background_hash.is_empty() && !background_hash.is_empty() {
                songs[index].background_hash = background_hash;
                songs[index].background_filename = background_filename;
            }
            continue;
        }
        seen.insert(hash.clone(), songs.len());
        songs.push(Song {
            hash,
            audio_filename: usage.filename.clone(),
            artist: meta.artist.clone(),
            title: meta.title.clone(),
            background_hash,
            background_filename,
        });
    }

    Ok(Survey {
        songs,
        beatmaps: beatmap_count,
        sets: set_files.len(),
    })
}

struct Meta {
    artist: String,
    title: String,
    audio: String,
    background: String,
}

struct Usage {
    filename: String,
    file_key: i64,
}

fn string_map(realm: &Realm, table: &str, column: &[&str]) -> Result<HashMap<i64, String>, String> {
    let mut out = HashMap::new();
    for (key, value) in realm.table(table)?.strings(column)? {
        if let Some(value) = value.filter(|text| !text.is_empty()) {
            out.insert(key, value);
        }
    }
    Ok(out)
}

fn meta_map(realm: &Realm) -> Result<HashMap<i64, Meta>, String> {
    let table = realm.table("class_BeatmapMetadata")?;
    let titles = maybe_strings(&table, &["Title"])?;
    let titles_u = maybe_strings(&table, &["TitleUnicode"])?;
    let artists = maybe_strings(&table, &["Artist"])?;
    let artists_u = maybe_strings(&table, &["ArtistUnicode"])?;
    let audio = column_strings(&table, &["AudioFile", "AudioFilename"])?;
    let background = maybe_strings(&table, &["BackgroundFile"])?;
    let mut out = HashMap::new();
    for (key, audio) in audio {
        if audio.trim().is_empty() {
            continue;
        }
        out.insert(
            key,
            Meta {
                title: prefer(&titles_u, &titles, key),
                artist: prefer(&artists_u, &artists, key),
                audio,
                background: background.get(&key).cloned().unwrap_or_default(),
            },
        );
    }
    Ok(out)
}

/// First usage whose name matches `BackgroundFile`. Empty when the map has none.
fn background_for(
    meta: &Meta,
    usage_keys: &[i64],
    usages: &HashMap<i64, Usage>,
    files: &HashMap<i64, String>,
) -> (String, String) {
    if meta.background.trim().is_empty() {
        return (String::new(), String::new());
    }
    let Some(usage) = usage_keys.iter().find_map(|key| {
        let usage = usages.get(key)?;
        names_match(&usage.filename, &meta.background).then_some(usage)
    }) else {
        return (String::new(), String::new());
    };
    let Some(hash) = files.get(&usage.file_key) else {
        return (String::new(), String::new());
    };
    let hash = normalize_hash(hash);
    if hash.len() != 64 {
        return (String::new(), String::new());
    }
    (hash, usage.filename.clone())
}

fn maybe_strings(
    table: &crate::realm::Table,
    names: &[&str],
) -> Result<HashMap<i64, String>, String> {
    if table.column(names).is_err() {
        return Ok(HashMap::new());
    }
    column_strings(table, names)
}

fn column_strings(
    table: &crate::realm::Table,
    names: &[&str],
) -> Result<HashMap<i64, String>, String> {
    let mut out = HashMap::new();
    for (key, value) in table.strings(names)? {
        if let Some(value) = value {
            out.insert(key, value);
        }
    }
    Ok(out)
}

fn prefer(primary: &HashMap<i64, String>, fallback: &HashMap<i64, String>, key: i64) -> String {
    primary
        .get(&key)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| fallback.get(&key))
        .cloned()
        .unwrap_or_default()
}

fn usage_map(realm: &Realm) -> Result<HashMap<i64, Usage>, String> {
    let table = realm.table("class_RealmNamedFileUsage")?;
    let names = column_strings(&table, &["Filename", "FileName"])?;
    let links = table.links(&["File"])?;
    let mut out = HashMap::new();
    for (key, file_key) in links {
        let Some(file_key) = file_key else { continue };
        let Some(filename) = names.get(&key).filter(|text| !text.is_empty()) else {
            continue;
        };
        out.insert(
            key,
            Usage {
                filename: filename.clone(),
                file_key,
            },
        );
    }
    Ok(out)
}

fn set_file_lists(realm: &Realm) -> Result<HashMap<i64, Vec<i64>>, String> {
    let table = realm.table("class_BeatmapSet")?;
    let column = table.column(&["Files"])?;
    let rows = if column.is_list() {
        table.link_lists(&["Files"])?
    } else {
        table
            .links(&["Files"])?
            .into_iter()
            .map(|(key, link)| (key, link.into_iter().collect()))
            .collect()
    };
    Ok(rows.into_iter().collect())
}

fn names_match(filename: &str, audio: &str) -> bool {
    let filename = filename.replace('\\', "/");
    let audio = audio.replace('\\', "/");
    if filename.eq_ignore_ascii_case(&audio) {
        return true;
    }
    let filename = filename.rsplit('/').next().unwrap_or(filename.as_str());
    let audio = audio.rsplit('/').next().unwrap_or(audio.as_str());
    filename.eq_ignore_ascii_case(audio)
}

fn normalize_hash(hash: &str) -> String {
    hash.trim().to_ascii_lowercase()
}

fn prefer_names(song: &mut Song, artist: &str, title: &str) {
    if song.artist.trim().is_empty() && !artist.trim().is_empty() {
        song.artist = artist.to_string();
    }
    if song.title.trim().is_empty() && !title.trim().is_empty() {
        song.title = title.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::hash_path;
    use std::path::PathBuf;

    #[test]
    fn names_match_ignores_directory_and_case() {
        assert!(names_match("audio.mp3", "Audio.mp3"));
        assert!(names_match("songs/audio.ogg", "audio.ogg"));
        assert!(!names_match("hit.wav", "audio.mp3"));
    }

    #[test]
    fn live_osu_realm_yields_beatmap_audio() {
        let path = std::env::temp_dir().join("osu-client.realm");
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let survey = survey(&bytes).expect("survey");
        assert!(!survey.songs.is_empty(), "beatmaps did not yield audio");
        assert!(survey.beatmaps > 0);
        for song in &survey.songs {
            assert_eq!(song.hash.len(), 64, "{}", song.hash);
            assert!(song.hash.chars().all(|ch| ch.is_ascii_hexdigit()));
        }
        let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) else {
            return;
        };
        let files = appdata.join("osu").join("files");
        if !files.is_dir() {
            return;
        }
        let found = survey
            .songs
            .iter()
            .filter(|song| hash_path(&files, &song.hash).is_file())
            .count();
        let missing = survey.songs.len() - found;
        assert!(
            missing * 10 <= survey.songs.len(),
            "{missing} of {} audio hashes are not in {}",
            survey.songs.len(),
            files.display()
        );
        let backgrounds: Vec<_> = survey
            .songs
            .iter()
            .filter(|song| {
                song.background_hash.len() == 64
                    && song.background_hash.chars().all(|ch| ch.is_ascii_hexdigit())
            })
            .collect();
        assert!(
            backgrounds.len() * 2 > survey.songs.len(),
            "{} of {} songs have a background",
            backgrounds.len(),
            survey.songs.len()
        );
        let bg_missing = backgrounds
            .iter()
            .filter(|song| !hash_path(&files, &song.background_hash).is_file())
            .count();
        assert!(
            bg_missing * 10 <= backgrounds.len(),
            "{bg_missing} of {} backgrounds are not in {}",
            backgrounds.len(),
            files.display()
        );
    }
}
