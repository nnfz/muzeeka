use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use image::imageops::FilterType;
use image::ImageFormat;
use lofty::picture::PictureType;
use lofty::prelude::*;
use rayon::prelude::*;
use rusqlite::{params, Connection};
use walkdir::WalkDir;

use muzeeka::audio::cue::{
    companion_cue_for_audio, covered_audio_paths, expand_cue_file_fast, CUE_PATH_MARKER,
};
use muzeeka::MusicFile;

use crate::ui::catalog;
use crate::ui::paths;

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "wav", "aac", "m4a", "wma", "opus", "ape", "mod", "s3m", "xm", "it",
    "ay", "ym", "vgm", "vgz", "nsf", "nsfe", "gbs", "hes", "sap", "kss", "pt2", "pt3", "stc",
    "stp", "asc", "sqt", "psg",
];

#[derive(Clone, Debug)]
pub struct DropSource {
    pub path: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug)]
pub enum ImportKind {
    Folders {
        sources: Vec<DropSource>,
    },
    Playlist {
        id: String,
        label: String,
        sources: Vec<DropSource>,
        at: Option<usize>,
    },
    NewPlaylist {
        sources: Vec<DropSource>,
    },
    Library {
        sources: Vec<DropSource>,
        at: Option<usize>,
    },
    Download {
        named: Option<String>,
        configured_id: Option<String>,
        output_dir: String,
    },
}

pub struct ImportReport {
    pub toast: String,
    pub open_id: Option<String>,
    pub remember_download_playlist: Option<String>,
}

struct Group {
    name: String,
    indexes: Vec<usize>,
}

pub fn scan_paths(paths: &[String]) -> Vec<MusicFile> {
    let mut discovered = Vec::new();
    let mut seen = HashSet::new();
    for raw in paths {
        let path = PathBuf::from(raw.trim());
        if path.as_os_str().is_empty() {
            continue;
        }
        let walk_root = paths::extended_path(&path);
        if path.is_dir() || walk_root.is_dir() {
            for entry in WalkDir::new(&walk_root)
                .follow_links(true)
                .into_iter()
                .flatten()
            {
                if entry.file_type().is_file() {
                    push_unique(&mut discovered, &mut seen, entry.into_path());
                }
            }
        } else if path.is_file() {
            push_unique(&mut discovered, &mut seen, path);
        }
    }
    build_files(discovered)
}

pub fn commit(
    db: &mut Connection,
    files: &[MusicFile],
    kind: &ImportKind,
) -> Result<ImportReport, String> {
    let files = dedupe_files(files);
    if files.is_empty() {
        return Ok(ImportReport {
            toast: "No supported audio files found".into(),
            open_id: None,
            remember_download_playlist: None,
        });
    }

    let tx = db
        .transaction()
        .map_err(|error| format!("Could not start import: {error}"))?;
    let report = match kind {
        ImportKind::Folders { sources } => {
            commit_groups(&tx, &files, &group_folder_drop(&files, sources), sources)?
        }
        ImportKind::NewPlaylist { sources } => {
            let name = new_playlist_name(&tx, sources)?;
            commit_groups(
                &tx,
                &files,
                &[Group {
                    name,
                    indexes: (0..files.len()).collect(),
                }],
                sources,
            )?
        }
        ImportKind::Playlist {
            id,
            label,
            sources,
            at,
        } => {
            let added = link_files(
                &tx,
                id,
                &files,
                &(0..files.len()).collect::<Vec<_>>(),
                sources,
                *at,
            )?;
            let toast = if added > 0 {
                format!("Added {added} {} to {label}", tracks_word(added))
            } else {
                "Tracks are already in this playlist".into()
            };
            ImportReport {
                toast,
                open_id: Some(id.clone()),
                remember_download_playlist: None,
            }
        }
        ImportKind::Library { sources, at } => commit_library(&tx, &files, sources, *at)?,
        ImportKind::Download {
            named,
            configured_id,
            output_dir,
        } => commit_download(
            &tx,
            &files,
            named.as_deref(),
            configured_id.as_deref(),
            output_dir,
        )?,
    };
    tx.commit()
        .map_err(|error| format!("Could not save the import: {error}"))?;
    Ok(report)
}

fn commit_library(
    tx: &Connection,
    files: &[MusicFile],
    sources: &[DropSource],
    mut at: Option<usize>,
) -> Result<ImportReport, String> {
    let indexes: Vec<usize> = (0..files.len()).collect();
    let roots = prepare_roots(tx, files, &indexes, sources)?;
    let mut added = 0usize;
    for file in files {
        let (_, _, key) = split_for_storage(&file.path, &roots)?;
        let exists = match tx.query_row("SELECT 1 FROM tracks WHERE path_key = ?1", [&key], |_| {
            Ok(())
        }) {
            Ok(()) => true,
            Err(rusqlite::Error::QueryReturnedNoRows) => false,
            Err(error) => return Err(error.to_string()),
        };
        let id = upsert_track(tx, file, &roots)?;
        if !exists {
            added += 1;
            if let Some(index) = at {
                place_library_row(tx, id, index)?;
                at = Some(index + 1);
            }
        }
    }
    let toast = if added > 0 {
        format!("Added {added} {}", tracks_word(added))
    } else {
        "Tracks are already in the library".into()
    };
    Ok(ImportReport {
        toast,
        open_id: None,
        remember_download_playlist: None,
    })
}

pub(crate) fn is_audio_drop(path: &str) -> bool {
    match extension_of(Path::new(path)).as_deref() {
        Some("cue") => true,
        Some(ext) => is_audio(ext),
        None => false,
    }
}

fn commit_download(
    tx: &Connection,
    files: &[MusicFile],
    named: Option<&str>,
    configured_id: Option<&str>,
    output_dir: &str,
) -> Result<ImportReport, String> {
    let sources = vec![DropSource {
        path: output_dir.to_string(),
        is_dir: true,
    }];
    let (id, remember) = if let Some(name) = named.map(str::trim).filter(|name| !name.is_empty()) {
        (ensure_playlist(tx, name)?, None)
    } else if let Some(id) = configured_id {
        if playlist_exists(tx, id)? {
            (id.to_string(), None)
        } else {
            let created = ensure_playlist(tx, "Downloads")?;
            (created.clone(), Some(created))
        }
    } else {
        let created = ensure_playlist(tx, "Downloads")?;
        (created.clone(), Some(created))
    };
    link_files(
        tx,
        &id,
        files,
        &(0..files.len()).collect::<Vec<_>>(),
        &sources,
        None,
    )?;
    Ok(ImportReport {
        toast: String::new(),
        open_id: Some(id),
        remember_download_playlist: remember,
    })
}

fn commit_groups(
    tx: &Connection,
    files: &[MusicFile],
    groups: &[Group],
    sources: &[DropSource],
) -> Result<ImportReport, String> {
    if groups.is_empty() {
        return Ok(ImportReport {
            toast: "No supported audio files found".into(),
            open_id: None,
            remember_download_playlist: None,
        });
    }
    let mut playlist_count = 0usize;
    let mut track_count = 0usize;
    let mut names = Vec::new();
    let mut last_id = None;
    for group in groups {
        if group.indexes.is_empty() {
            continue;
        }
        let id = ensure_playlist(tx, &group.name)?;
        track_count += link_files(tx, &id, files, &group.indexes, sources, None)?;
        playlist_count += 1;
        names.push(group.name.clone());
        last_id = Some(id);
    }
    Ok(ImportReport {
        toast: toast_for_create(playlist_count, track_count, &names),
        open_id: last_id,
        remember_download_playlist: None,
    })
}

fn toast_for_create(playlists: usize, tracks: usize, names: &[String]) -> String {
    if playlists == 0 {
        return "No supported audio files found".into();
    }
    if playlists == 1 {
        let name = names.first().map(String::as_str).unwrap_or("playlist");
        return if tracks > 0 {
            format!("Created «{name}» · {tracks} {}", tracks_word(tracks))
        } else {
            format!("Opened «{name}» (tracks already in playlist)")
        };
    }
    format!(
        "Created {playlists} playlists · {tracks} {}",
        tracks_word(tracks)
    )
}

fn tracks_word(count: usize) -> &'static str {
    if count == 1 {
        "track"
    } else {
        "tracks"
    }
}

fn group_folder_drop(files: &[MusicFile], sources: &[DropSource]) -> Vec<Group> {
    let dirs: Vec<&DropSource> = sources.iter().filter(|source| source.is_dir).collect();
    if dirs.is_empty() {
        return vec![Group {
            name: String::new(),
            indexes: (0..files.len()).collect(),
        }];
    }
    let mut claimed = HashSet::new();
    let mut groups = Vec::new();
    for source in &dirs {
        let prefix = paths::normalize_prefix(&source.path);
        if prefix.is_empty() {
            continue;
        }
        let indexes: Vec<usize> = files
            .iter()
            .enumerate()
            .filter(|(index, file)| !claimed.contains(index) && file_under(file, &prefix))
            .map(|(index, _)| index)
            .collect();
        if indexes.is_empty() {
            continue;
        }
        for index in &indexes {
            claimed.insert(*index);
        }
        let name = paths::basename(&source.path);
        groups.push(Group {
            name: if name.is_empty() { String::new() } else { name },
            indexes,
        });
    }
    let rest: Vec<usize> = (0..files.len())
        .filter(|index| !claimed.contains(index))
        .collect();
    if rest.is_empty() {
        return finalize_names(groups);
    }
    if dirs.len() == 1 {
        let name = paths::basename(&dirs[0].path);
        if let Some(group) = groups.first_mut() {
            group.indexes.extend(rest);
        } else {
            groups.push(Group {
                name,
                indexes: rest,
            });
        }
    } else if groups.is_empty() {
        groups.push(Group {
            name: String::new(),
            indexes: rest,
        });
    } else if let Some(group) = groups.last_mut() {
        group.indexes.extend(rest);
    }
    finalize_names(groups)
}

fn finalize_names(mut groups: Vec<Group>) -> Vec<Group> {
    for (index, group) in groups.iter_mut().enumerate() {
        if group.name.trim().is_empty() {
            group.name = format!("Playlist {}", index + 1);
        }
    }
    groups
}

fn file_under(file: &MusicFile, prefix: &str) -> bool {
    let file_key = paths::normalize_prefix(
        file.path
            .split(CUE_PATH_MARKER)
            .next()
            .unwrap_or(&file.path),
    );
    let audio_key = file
        .audio_path
        .as_deref()
        .map(paths::normalize_prefix)
        .unwrap_or_default();
    under_prefix(&file_key, prefix) || (!audio_key.is_empty() && under_prefix(&audio_key, prefix))
}

fn under_prefix(key: &str, prefix: &str) -> bool {
    key == prefix
        || key.starts_with(&format!("{prefix}\\"))
        || key.starts_with(&format!("{prefix}/"))
}

fn new_playlist_name(tx: &Connection, sources: &[DropSource]) -> Result<String, String> {
    let dirs: Vec<&DropSource> = sources.iter().filter(|source| source.is_dir).collect();
    if dirs.len() == 1 {
        let name = paths::basename(&dirs[0].path);
        if !name.is_empty() {
            return Ok(name);
        }
    }
    next_playlist_name(tx)
}

fn next_playlist_name(tx: &Connection) -> Result<String, String> {
    let names = playlist_names(tx)?;
    let mut index = names.len() + 1;
    loop {
        let name = format!("Playlist {index}");
        if !names.iter().any(|existing| existing == &name) {
            return Ok(name);
        }
        index += 1;
    }
}

fn ensure_playlist(tx: &Connection, name: &str) -> Result<String, String> {
    let name = name.trim();
    let name = if name.is_empty() {
        next_playlist_name(tx)?
    } else {
        name.to_string()
    };
    if let Some(id) = find_playlist_id(tx, &name)? {
        return Ok(id);
    }
    let id = new_playlist_id();
    tx.execute(
        "INSERT INTO playlists(id, name, position)
         VALUES (?1, ?2, (SELECT COALESCE(MAX(position), -1) + 1 FROM playlists))",
        params![id, name],
    )
    .map_err(|error| format!("Could not save playlist: {error}"))?;
    Ok(id)
}

fn find_playlist_id(tx: &Connection, name: &str) -> Result<Option<String>, String> {
    let mut statement = tx
        .prepare("SELECT id, name FROM playlists")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    for row in rows {
        let (id, existing) = row.map_err(|error| error.to_string())?;
        if existing.eq_ignore_ascii_case(name) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn playlist_exists(tx: &Connection, id: &str) -> Result<bool, String> {
    match tx.query_row("SELECT 1 FROM playlists WHERE id = ?1", [id], |_| Ok(())) {
        Ok(()) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn playlist_names(tx: &Connection) -> Result<Vec<String>, String> {
    let mut statement = tx
        .prepare("SELECT name FROM playlists")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

fn new_playlist_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{ms:x}-{n:x}")
}

fn link_files(
    tx: &Connection,
    playlist_id: &str,
    files: &[MusicFile],
    indexes: &[usize],
    sources: &[DropSource],
    mut at: Option<usize>,
) -> Result<usize, String> {
    let roots = prepare_roots(tx, files, indexes, sources)?;
    let mut added = 0usize;
    for index in indexes {
        let Some(file) = files.get(*index) else {
            continue;
        };
        let track_id = upsert_track(tx, file, &roots)?;
        if playlist_has_track(tx, playlist_id, track_id)? {
            continue;
        }
        let position = match at {
            Some(index) => {
                tx.execute(
                    "UPDATE playlist_tracks SET position = position + 1
                      WHERE playlist_id = ?1 AND position >= ?2",
                    params![playlist_id, index as i64],
                )
                .map_err(|error| format!("Could not add to playlist: {error}"))?;
                at = Some(index + 1);
                index as i64
            }
            None => tx
                .query_row(
                    "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_tracks WHERE playlist_id = ?1",
                    [playlist_id],
                    |row| row.get(0),
                )
                .map_err(|error| format!("Could not add to playlist: {error}"))?,
        };
        tx.execute(
            "INSERT INTO playlist_tracks(playlist_id, track_id, position) VALUES (?1, ?2, ?3)",
            params![playlist_id, track_id, position],
        )
        .map_err(|error| format!("Could not add to playlist: {error}"))?;
        added += 1;
    }
    Ok(added)
}

fn playlist_has_track(tx: &Connection, playlist_id: &str, track_id: i64) -> Result<bool, String> {
    match tx.query_row(
        "SELECT 1 FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2",
        params![playlist_id, track_id],
        |_| Ok(()),
    ) {
        Ok(()) => Ok(true),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn place_library_row(tx: &Connection, track_id: i64, at: usize) -> Result<(), String> {
    tx.execute(
        "UPDATE tracks SET library_position = library_position + 1
          WHERE id != ?2 AND library_position IS NOT NULL AND library_position >= ?1",
        params![at as i64, track_id],
    )
    .map_err(|error| format!("Could not place the track: {error}"))?;
    tx.execute(
        "UPDATE tracks SET library_position = ?1 WHERE id = ?2",
        params![at as i64, track_id],
    )
    .map_err(|error| format!("Could not place the track: {error}"))?;
    Ok(())
}

fn prepare_roots(
    tx: &Connection,
    files: &[MusicFile],
    indexes: &[usize],
    sources: &[DropSource],
) -> Result<Vec<(i64, String)>, String> {
    let mut parents = HashSet::new();
    for source in sources {
        if source.is_dir {
            parents.insert(paths::normalize_display_path(&source.path));
            continue;
        }
        if let Some(parent) = Path::new(&source.path).parent() {
            if !parent.as_os_str().is_empty() {
                parents.insert(paths::normalize_display_path(&parent.to_string_lossy()));
            }
        }
    }
    if parents.is_empty() {
        for index in indexes {
            let Some(file) = files.get(*index) else {
                continue;
            };
            let anchor = file
                .audio_path
                .as_deref()
                .filter(|path| !path.is_empty())
                .unwrap_or(
                    file.path
                        .split(CUE_PATH_MARKER)
                        .next()
                        .unwrap_or(&file.path),
                );
            if let Some(parent) = Path::new(anchor).parent() {
                if !parent.as_os_str().is_empty() {
                    parents.insert(paths::normalize_display_path(&parent.to_string_lossy()));
                }
            }
        }
    }
    for parent in parents {
        if parent.is_empty() {
            continue;
        }
        ensure_root(tx, &parent)?;
    }
    load_roots_longest(tx)
}

fn ensure_root(tx: &Connection, absolute: &str) -> Result<i64, String> {
    let cleaned = paths::normalize_display_path(absolute)
        .trim_end_matches(['/', '\\'])
        .to_string();
    if cleaned.is_empty() {
        return Err("Root path is empty".into());
    }
    let key = paths::path_key(&cleaned);
    match tx.query_row(
        "SELECT id FROM library_roots WHERE path_key = ?1",
        [&key],
        |row| row.get::<_, i64>(0),
    ) {
        Ok(id) => {
            let _ = tx.execute(
                "UPDATE library_roots SET path = ?2 WHERE id = ?1",
                params![id, cleaned],
            );
            Ok(id)
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            tx.execute(
                "INSERT INTO library_roots(path, path_key) VALUES (?1, ?2)",
                params![cleaned, key],
            )
            .map_err(|error| format!("Could not save the library folder: {error}"))?;
            Ok(tx.last_insert_rowid())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn load_roots_longest(tx: &Connection) -> Result<Vec<(i64, String)>, String> {
    let mut statement = tx
        .prepare("SELECT id, path FROM library_roots")
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut roots = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    roots.sort_by(|left, right| {
        paths::path_key(&right.1)
            .len()
            .cmp(&paths::path_key(&left.1).len())
    });
    Ok(roots)
}

fn upsert_track(
    tx: &Connection,
    track: &MusicFile,
    roots: &[(i64, String)],
) -> Result<i64, String> {
    if track.path.trim().is_empty() {
        return Err("Cannot store a track with an empty path".into());
    }
    let (root_id, rel_path, key) = split_for_storage(&track.path, roots)?;
    let audio_rel = track
        .audio_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(|audio| split_for_storage(audio, roots).map(|(_, rel, _)| rel))
        .transpose()?;
    let cover_id = cover_id_from_paths(
        track.cover_path.as_deref(),
        track.cover_path_full.as_deref(),
    );
    tx.query_row(
        "INSERT INTO tracks(
             root_id, rel_path, path_key, file_name, extension, size, title, artist, album,
             duration_secs, year, track_number, genre, cover_id, audio_rel_path,
             cue_start_secs, cue_end_secs, library_position
         ) VALUES (
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
             ?16, ?17, (SELECT COALESCE(MAX(library_position), -1) + 1 FROM tracks)
         )
         ON CONFLICT(path_key) DO UPDATE SET
             root_id = excluded.root_id,
             rel_path = excluded.rel_path,
             file_name = excluded.file_name,
             extension = excluded.extension,
             size = excluded.size,
             title = COALESCE(excluded.title, tracks.title),
             artist = COALESCE(excluded.artist, tracks.artist),
             album = COALESCE(excluded.album, tracks.album),
             duration_secs = COALESCE(excluded.duration_secs, tracks.duration_secs),
             year = COALESCE(excluded.year, tracks.year),
             track_number = COALESCE(excluded.track_number, tracks.track_number),
             genre = COALESCE(excluded.genre, tracks.genre),
             cover_id = COALESCE(excluded.cover_id, tracks.cover_id),
             audio_rel_path = COALESCE(excluded.audio_rel_path, tracks.audio_rel_path),
             cue_start_secs = COALESCE(excluded.cue_start_secs, tracks.cue_start_secs),
             cue_end_secs = COALESCE(excluded.cue_end_secs, tracks.cue_end_secs)
         RETURNING id",
        params![
            root_id,
            rel_path,
            key,
            track.file_name,
            track.extension,
            i64::try_from(track.size).unwrap_or(i64::MAX),
            blank_to_none(track.title.as_deref()),
            blank_to_none(track.artist.as_deref()),
            blank_to_none(track.album.as_deref()),
            track.duration_secs,
            track.year.map(i64::from),
            track.track_number.map(i64::from),
            blank_to_none(track.genre.as_deref()),
            cover_id,
            audio_rel,
            track.cue_start_secs,
            track.cue_end_secs,
        ],
        |row| row.get(0),
    )
    .map_err(|error| format!("Could not save track: {error}"))
}

fn split_for_storage(
    absolute: &str,
    roots: &[(i64, String)],
) -> Result<(Option<i64>, String, String), String> {
    if paths::is_stream_url(absolute) {
        let url = absolute.trim().to_string();
        let key = paths::path_key(&url);
        return Ok((None, url, key));
    }
    let abs = paths::normalize_display_path(absolute);
    for (id, root) in roots {
        if let Some(rel) = paths::to_relative(root, &abs) {
            return Ok((
                Some(*id),
                rel.clone(),
                paths::storage_path_key(Some(*id), &rel),
            ));
        }
    }
    Ok((None, abs.clone(), paths::storage_path_key(None, &abs)))
}

fn blank_to_none(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn cover_id_from_paths(thumb: Option<&str>, full: Option<&str>) -> Option<String> {
    thumb
        .and_then(cover_id_from_path)
        .or_else(|| full.and_then(cover_id_from_path))
}

pub(crate) fn cover_id_from_path(path: &str) -> Option<String> {
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path.trim());
    let lower = name.to_ascii_lowercase();
    let rest = lower.strip_prefix("c-")?;
    let (id, suffix) = rest.split_once('-')?;
    if id.len() == 16
        && id.chars().all(|ch| ch.is_ascii_hexdigit())
        && (suffix.starts_with("thumb.") || suffix.starts_with("full."))
    {
        Some(id.to_string())
    } else {
        None
    }
}

fn push_unique(paths: &mut Vec<PathBuf>, seen: &mut HashSet<String>, path: PathBuf) {
    let key = paths::path_key(&path.to_string_lossy());
    if seen.insert(key) {
        paths.push(path);
    }
}

fn build_files(paths: Vec<PathBuf>) -> Vec<MusicFile> {
    let mut cue_paths = Vec::new();
    let mut audio_paths = Vec::new();
    for path in &paths {
        match extension_of(path).as_deref() {
            Some("cue") => cue_paths.push(path.clone()),
            Some(ext) if is_audio(ext) => audio_paths.push(path.clone()),
            None if sniff_extension(path).is_some() => audio_paths.push(path.clone()),
            _ => {}
        }
    }
    if cue_paths.is_empty() {
        for audio in &audio_paths {
            if let Some(cue) = companion_cue_for_audio(audio) {
                let key = paths::path_key(&cue.to_string_lossy());
                if cue_paths
                    .iter()
                    .all(|existing| paths::path_key(&existing.to_string_lossy()) != key)
                {
                    cue_paths.push(cue);
                }
            }
        }
    }
    let mut files = Vec::new();
    for cue in &cue_paths {
        files.extend(expand_cue_file_fast(cue));
    }
    paint_cue_covers(&mut files);
    let covered = covered_audio_paths(&cue_paths)
        .iter()
        .map(|path| paths::path_key(path))
        .collect::<HashSet<_>>();
    let pending: Vec<PathBuf> = audio_paths
        .into_iter()
        .filter(|path| !is_covered(path, &covered))
        .collect();
    let covers = catalog::app_data_dir().join("covers");
    let _ = fs::create_dir_all(&covers);
    let tagged: Vec<MusicFile> = pending
        .par_iter()
        .filter_map(|path| read_audio(path, &covers))
        .collect();
    files.extend(tagged);
    dedupe_files(&files)
}

fn dedupe_files(files: &[MusicFile]) -> Vec<MusicFile> {
    let mut out = Vec::with_capacity(files.len());
    let mut seen = HashSet::new();
    for file in files {
        if file.path.trim().is_empty() {
            continue;
        }
        if seen.insert(paths::path_key(&file.path)) {
            out.push(file.clone());
        }
    }
    out
}

fn is_covered(path: &Path, covered: &HashSet<String>) -> bool {
    let text = path.to_string_lossy();
    if covered.contains(&paths::path_key(&text)) {
        return true;
    }
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    covered.contains(&paths::path_key(&canonical.to_string_lossy()))
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

fn is_audio(ext: &str) -> bool {
    AUDIO_EXTENSIONS.contains(&ext)
}

fn sniff_extension(path: &Path) -> Option<String> {
    if extension_of(path).is_some() {
        return None;
    }
    let mut file = fs::File::open(path).ok()?;
    let mut head = [0u8; 16];
    use std::io::Read;
    let n = file.read(&mut head).ok()?;
    sniff_audio_magic(&head[..n]).map(str::to_string)
}

fn sniff_audio_magic(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"ID3") || (head.len() >= 2 && head[0] == 0xFF && (head[1] & 0xE0) == 0xE0)
    {
        return Some("mp3");
    }
    if head.starts_with(b"OggS") {
        return Some("ogg");
    }
    if head.starts_with(b"fLaC") {
        return Some("flac");
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        return Some("wav");
    }
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        return Some("m4a");
    }
    None
}

#[derive(Clone, Copy)]
pub(crate) struct BlankName<'a> {
    pub path: &'a str,
    pub title: &'a str,
    pub artist: &'a str,
    pub cover: &'a str,
}

pub(crate) fn fill_blank_names(files: &mut [MusicFile], labels: &[BlankName<'_>]) {
    let covers = catalog::app_data_dir().join("covers");
    apply_labels(files, labels, &covers);
}

fn apply_labels(files: &mut [MusicFile], labels: &[BlankName<'_>], covers: &Path) {
    if labels.is_empty() || files.is_empty() {
        return;
    }
    let mut by_key = HashMap::new();
    for label in labels {
        by_key.insert(paths::path_key(label.path), *label);
    }
    let mut covers_ready = false;
    for file in files.iter_mut() {
        let Some(label) = by_key.get(&paths::path_key(&file.path)) else {
            continue;
        };
        if name_blank(&file.title) && !label.title.trim().is_empty() {
            file.title = Some(label.title.trim().to_string());
        }
        if name_blank(&file.artist) && !label.artist.trim().is_empty() {
            file.artist = Some(label.artist.trim().to_string());
        }
        let cover = label.cover.trim();
        if file.cover_path.is_none() && !cover.is_empty() {
            if !covers_ready {
                if fs::create_dir_all(covers).is_err() {
                    continue;
                }
                covers_ready = true;
            }
            if let Some((thumb, full)) = store_image_file(Path::new(cover), covers) {
                file.cover_path = Some(thumb);
                file.cover_path_full = Some(full);
            }
        }
    }
}

fn name_blank(value: &Option<String>) -> bool {
    value
        .as_ref()
        .map(|text| text.trim().is_empty())
        .unwrap_or(true)
}

fn read_audio(path: &Path, covers: &Path) -> Option<MusicFile> {
    let ext = match extension_of(path) {
        Some(ext) if is_audio(&ext) => ext,
        Some(_) => return None,
        None => sniff_extension(path)?,
    };
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    if file_name.is_empty() {
        return None;
    }
    let size = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let full = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut file = MusicFile {
        path: full.to_string_lossy().to_string(),
        file_name,
        extension: ext,
        size,
        title: None,
        artist: None,
        album: None,
        duration: 0.0,
        duration_secs: None,
        year: None,
        track_number: None,
        genre: None,
        cover_path: None,
        cover_path_full: None,
        audio_path: None,
        cue_start_secs: None,
        cue_end_secs: None,
    };
    apply_tags(&mut file, path, covers);
    Some(file)
}

fn apply_tags(file: &mut MusicFile, path: &Path, covers: &Path) {
    let Some(tagged) = read_tagged(path) else {
        return;
    };
    let duration = tagged.properties().duration().as_secs_f64();
    if duration.is_finite() && duration > 0.0 {
        file.duration = duration;
        file.duration_secs = Some(duration);
    }
    let Some(tag) = tagged.primary_tag() else {
        return;
    };
    file.title = owned_text(tag.title());
    file.artist = owned_text(tag.artist());
    file.album = owned_text(tag.album());
    file.genre = owned_text(tag.genre());
    file.track_number = tag.track();
    file.year = tag
        .get_string(ItemKey::Year)
        .or_else(|| tag.get_string(ItemKey::RecordingDate))
        .and_then(parse_year);
    if let Some((thumb, full)) = store_cover(tag, covers) {
        file.cover_path = Some(thumb);
        file.cover_path_full = Some(full);
    }
}

fn owned_text(value: Option<impl ToString>) -> Option<String> {
    value
        .map(|text| text.to_string())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn parse_year(text: &str) -> Option<u32> {
    let digits: String = text
        .chars()
        .take(4)
        .filter(|ch| ch.is_ascii_digit())
        .collect();
    if digits.len() == 4 {
        digits.parse().ok()
    } else {
        None
    }
}

pub(crate) fn stored_cover(path: &Path) -> Option<(String, PathBuf)> {
    let covers = catalog::app_data_dir().join("covers");
    let _ = fs::create_dir_all(&covers);
    let (thumb, _) = stored_cover_paths(path, &covers)?;
    let id = cover_id_from_path(&thumb)?;
    Some((id, PathBuf::from(thumb)))
}

fn stored_cover_paths(path: &Path, covers: &Path) -> Option<(String, String)> {
    let tagged = read_tagged(path)?;
    let tag = tagged.primary_tag()?;
    store_cover(tag, covers)
}

fn read_tagged(path: &Path) -> Option<lofty::file::TaggedFile> {
    if path.extension().is_some() {
        return lofty::read_from_path(path).ok();
    }
    let mut file = fs::File::open(path).ok()?;
    lofty::read_from(&mut file).ok()
}

fn paint_cue_covers(files: &mut [MusicFile]) {
    let covers = catalog::app_data_dir().join("covers");
    let _ = fs::create_dir_all(&covers);
    let mut cache: HashMap<String, Option<(String, String)>> = HashMap::new();
    for file in files.iter_mut() {
        if file.cover_path.is_some() || !file.path.contains(CUE_PATH_MARKER) {
            continue;
        }
        let Some(audio) = file.audio_path.clone() else {
            continue;
        };
        let found = cache
            .entry(audio.clone())
            .or_insert_with(|| stored_cover_paths(Path::new(&audio), &covers));
        if let Some((thumb, full)) = found.clone() {
            file.cover_path = Some(thumb);
            file.cover_path_full = Some(full);
        }
    }
}

fn store_cover(tag: &lofty::tag::Tag, covers: &Path) -> Option<(String, String)> {
    let picture = tag
        .pictures()
        .iter()
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| tag.pictures().first())?;
    store_cover_bytes(picture.data(), covers)
}

fn store_image_file(path: &Path, covers: &Path) -> Option<(String, String)> {
    let bytes = fs::read(path).ok()?;
    store_cover_bytes(&bytes, covers)
}

fn store_cover_bytes(bytes: &[u8], covers: &Path) -> Option<(String, String)> {
    if bytes.is_empty() {
        return None;
    }
    let id = cover_content_id(bytes);
    let thumb = covers.join(format!("c-{id}-thumb.webp"));
    let full = covers.join(format!("c-{id}-full.jpg"));
    if !thumb.is_file() {
        let image = image::load_from_memory(bytes).ok()?;
        let (width, height) = image::GenericImageView::dimensions(&image);
        let thumb_image = if width <= 96 && height <= 96 {
            image.clone()
        } else {
            image.thumbnail(96, 96)
        };
        thumb_image
            .save_with_format(&thumb, ImageFormat::WebP)
            .ok()?;
        if !full.is_file() {
            let full_image = if width <= 1400 && height <= 1400 {
                image
            } else {
                image.resize(1400, 1400, FilterType::Triangle)
            };
            let _ = full_image.save_with_format(&full, ImageFormat::Jpeg);
        }
    }
    thumb.is_file().then(|| {
        (
            thumb.to_string_lossy().to_string(),
            full.to_string_lossy().to_string(),
        )
    })
}

fn cover_content_id(data: &[u8]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash ^= data.len() as u64;
    hash = hash.wrapping_mul(0x100000001b3);
    format!("{hash:016x}")
}

pub fn classify_drops(paths: &[String]) -> Vec<DropSource> {
    paths
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .map(|path| DropSource {
            is_dir: Path::new(path).is_dir(),
            path: path.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> MusicFile {
        MusicFile {
            path: path.into(),
            file_name: paths::basename(path),
            extension: "mp3".into(),
            size: 1,
            title: None,
            artist: None,
            album: None,
            duration: 1.0,
            duration_secs: Some(1.0),
            year: None,
            track_number: None,
            genre: None,
            cover_path: None,
            cover_path_full: None,
            audio_path: None,
            cue_start_secs: None,
            cue_end_secs: None,
        }
    }

    #[test]
    fn one_folder_keeps_unmatched_files_in_that_playlist() {
        let files = vec![
            file(r"D:\Albums\Night\01.mp3"),
            file(r"D:\Somewhere\else.mp3"),
        ];
        let sources = vec![DropSource {
            path: r"D:\Albums\Night".into(),
            is_dir: true,
        }];
        let groups = group_folder_drop(&files, &sources);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "Night");
        assert_eq!(groups[0].indexes, vec![0, 1]);
    }

    #[test]
    fn loose_files_become_one_playlist() {
        let files = vec![file(r"D:\a.mp3"), file(r"D:\b.mp3")];
        let sources = vec![
            DropSource {
                path: r"D:\a.mp3".into(),
                is_dir: false,
            },
            DropSource {
                path: r"D:\b.mp3".into(),
                is_dir: false,
            },
        ];
        let groups = group_folder_drop(&files, &sources);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].indexes.len(), 2);
    }

    #[test]
    fn a_dropped_folder_includes_files_nested_several_levels_down() {
        let root = std::env::temp_dir().join(format!("muzeeka-nest-{}", std::process::id()));
        let deep = root.join("Artist").join("Album").join("CD1").join("Disc");
        std::fs::create_dir_all(&deep).unwrap();
        let track = deep.join("01 song.mp3");
        std::fs::write(&track, b"not-audio").unwrap();
        let files = scan_paths(&[root.to_string_lossy().to_string()]);
        assert!(
            files
                .iter()
                .any(|file| paths::path_key(&file.path).ends_with("01 song.mp3")),
            "nested track was not scanned: {files:?}"
        );
        let sources = vec![DropSource {
            path: root.to_string_lossy().to_string(),
            is_dir: true,
        }];
        let groups = group_folder_drop(&files, &sources);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].indexes.len(), files.len());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extensionless_audio_is_imported_from_its_header() {
        let root = std::env::temp_dir().join(format!("muzeeka-raw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let audio = root.join("47b895484e7751f3ab429694ff6dbf21");
        let junk = root.join("not-audio");
        std::fs::write(&audio, b"ID3\x03\x00\x00\x00\x00\x00\x00").unwrap();
        std::fs::write(&junk, b"hello").unwrap();
        let files = scan_paths(&[
            audio.to_string_lossy().to_string(),
            junk.to_string_lossy().to_string(),
        ]);
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].extension, "mp3");
        let mut tagged = files.clone();
        tagged[0].title = Some("from the file".into());
        let audio_text = audio.to_string_lossy().to_string();
        fill_blank_names(
            &mut tagged,
            &[BlankName {
                path: &audio_text,
                title: "from the map",
                artist: "cYsmix",
                cover: "",
            }],
        );
        assert_eq!(tagged[0].title.as_deref(), Some("from the file"));
        assert_eq!(tagged[0].artist.as_deref(), Some("cYsmix"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn background_fills_a_blank_cover_and_leaves_an_existing_one() {
        let root = std::env::temp_dir().join(format!("muzeeka-bg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let covers = root.join("covers");
        std::fs::create_dir_all(&covers).unwrap();
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            2,
            image::Rgb([12, 34, 56]),
        ));
        let mut bytes = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Jpeg)
            .unwrap();
        let background = root.join("bg");
        std::fs::write(&background, &bytes).unwrap();
        let background_text = background.to_string_lossy().to_string();
        let mut open = vec![file(r"D:\osu\song")];
        apply_labels(
            &mut open,
            &[BlankName {
                path: r"D:\osu\song",
                title: "",
                artist: "",
                cover: &background_text,
            }],
            &covers,
        );
        let thumb = open[0].cover_path.as_deref().expect("cover");
        assert!(thumb.contains("thumb.webp"), "{thumb}");
        assert!(Path::new(thumb).is_file());
        assert!(Path::new(open[0].cover_path_full.as_deref().unwrap()).is_file());

        let mut kept = vec![file(r"D:\osu\song")];
        kept[0].cover_path = Some("already".into());
        apply_labels(
            &mut kept,
            &[BlankName {
                path: r"D:\osu\song",
                title: "map",
                artist: "",
                cover: &background_text,
            }],
            &covers,
        );
        assert_eq!(kept[0].cover_path.as_deref(), Some("already"));
        assert_eq!(kept[0].title.as_deref(), Some("map"));

        let junk = root.join("not-image");
        std::fs::write(&junk, b"hello").unwrap();
        let junk_text = junk.to_string_lossy().to_string();
        let mut bare = vec![file(r"D:\osu\other")];
        apply_labels(
            &mut bare,
            &[BlankName {
                path: r"D:\osu\other",
                title: "",
                artist: "",
                cover: &junk_text,
            }],
            &covers,
        );
        assert!(bare[0].cover_path.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cover_id_reads_thumb_name() {
        let id = cover_id_from_path(r"C:\covers\c-0123456789abcdef-thumb.webp");
        assert_eq!(id.as_deref(), Some("0123456789abcdef"));
    }
}
