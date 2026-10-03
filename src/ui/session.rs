// UI session for the main window.
//
// The track list is the library in `%APPDATA%\com.nnfz.muzeeka`. `Session::open`
// plays through BASS. `preview` and `empty` keep the clock so tests stay quiet.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use muzeeka::audio::player::{GaplessTrack, PlaybackState};

use gpui::SharedString;
use rusqlite::Connection;

use crate::ui::catalog::{self, LoadedLibrary};
use crate::ui::playback::{self, AudioRack, Engine};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TrackId(pub u32);

#[derive(Clone, Debug)]
pub struct Track {
    pub id: TrackId,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
    pub liked: bool,
    /// Absolute file path, including a `#cue:N` suffix when the row is a cue entry.
    pub path: String,
    pub cover: Option<PathBuf>,
    /// Cached `.ttml` in the shared lyrics folder, when this track has one.
    pub lyrics: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub track_ids: Vec<TrackId>,
    /// Picture shown in the sidebar. A user cover, or the first track's cover.
    pub cover: Option<PathBuf>,
    /// `playlists.cover_path` when the user set one. Empty means the picture is a fallback.
    pub cover_path: Option<String>,
    pub mix_mode: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryView {
    All,
    Liked,
    Playlist(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepeatMode {
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Title,
    Album,
    Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: SortColumn,
    pub descending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchField {
    Both,
    Title,
    Artist,
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub label: SharedString,
    pub detail: SharedString,
    pub insert: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    General,
    Downloads,
    Plugins,
    Audio,
    About,
}

/// How regular playlists are drawn in the sidebar. All tracks and Liked stay compact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistDensity {
    Normal,
    Compact,
    Nano,
}

impl PlaylistDensity {
    pub const ALL: [Self; 3] = [Self::Normal, Self::Compact, Self::Nano];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Compact => "Compact",
            Self::Nano => "Nano",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Normal => "Cover, name, and track count",
            Self::Compact => "Same size as All tracks and Liked",
            Self::Nano => "Playlist name only",
        }
    }
}

impl SettingsSection {
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::Downloads,
        Self::Plugins,
        Self::Audio,
        Self::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Downloads => "Downloads",
            Self::Plugins => "Plugins",
            Self::Audio => "Audio",
            Self::About => "About",
        }
    }
}

pub struct Session {
    tracks: Vec<Track>,
    pub playlists: Vec<Playlist>,
    pub view: LibraryView,
    pub current: Option<TrackId>,
    pub playing: bool,
    pub position: f64,
    pub volume: f32,
    pub last_volume: f32,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub sort: Option<Sort>,
    pub queue: Vec<TrackId>,
    /// Shuffle play order. The current track stays at the front when the
    /// mode is turned on, so the followers can be preloaded.
    shuffle_order: Vec<TrackId>,
    rng: u32,
    pub discord_rpc: bool,
    pub auto_video_bg: bool,
    pub shuffle_smart: bool,
    pub playlist_density: PlaylistDensity,
    pub notice: Option<String>,
    index_by_id: HashMap<TrackId, usize>,
    playlist_of: HashMap<TrackId, String>,
    /// Lowercased title, artist, and album, parallel to `tracks`.
    /// Built once so a search does not allocate a folded copy of every hit.
    search_folds: Vec<SearchFold>,
    /// Live output. Absent in tests and when BASS failed to open.
    pub(crate) engine: Option<Engine>,
    /// Effect rack, speed, and presets from settings.json.
    pub rack: AudioRack,
    /// When the current file was handed to BASS.
    armed_at: Option<Instant>,
    /// Furthest playhead seen on this file. A decoder that stops by rewinding
    /// to zero still counts as the end once this is near the duration.
    heard_to: f64,
    /// User pressed pause. A stopped stream must not advance past that.
    hold: bool,
    db: Option<Connection>,
}

struct SearchFold {
    id: TrackId,
    title: String,
    artist: String,
    album: String,
}

impl Session {
    /// Real library from the shared app-data folder. Sample tracks are only a
    /// fallback when that database is not installed.
    pub fn open() -> Self {
        let mut session = match catalog::load() {
            Ok(library) => Self::from_library(library),
            Err(error) => {
                let mut session = if catalog::app_data_dir().join("library.db").is_file() {
                    Self::empty()
                } else {
                    Self::preview()
                };
                session.notice = Some(error);
                session
            }
        };
        session.rack = AudioRack::load();
        if let Some(warning) = session.rack.warning.clone() {
            session.push_notice(warning);
        }
        match Engine::attach(session.volume, &session.rack) {
            Ok(engine) => {
                if let Some(warning) = engine.warning.clone() {
                    session.push_notice(warning);
                }
                session.engine = Some(engine);
            }
            Err(error) => session.push_notice(format!("Audio output is unavailable: {error}")),
        }
        session
    }

    pub(crate) fn push_notice(&mut self, line: String) {
        self.notice = Some(match self.notice.take() {
            Some(existing) => format!("{existing} {line}"),
            None => line,
        });
    }

    fn from_library(library: LoadedLibrary) -> Self {
        let volume = library.volume;
        let mut session = Self {
            tracks: library.tracks,
            playlists: library.playlists,
            view: library.view,
            current: library.current,
            playing: false,
            position: library.position,
            volume,
            last_volume: volume,
            shuffle: library.shuffle,
            repeat: library.repeat,
            sort: None,
            queue: Vec::new(),
            shuffle_order: Vec::new(),
            rng: 0x1234_5678,
            discord_rpc: true,
            auto_video_bg: false,
            shuffle_smart: true,
            playlist_density: PlaylistDensity::Normal,
            notice: None,
            index_by_id: HashMap::new(),
            playlist_of: HashMap::new(),
            search_folds: Vec::new(),
            engine: None,
            rack: AudioRack::flat(),
            armed_at: None,
            heard_to: 0.0,
            hold: false,
            db: Some(library.db),
        };
        session.rebuild_indexes();
        session
    }

    fn empty() -> Self {
        Self {
            tracks: Vec::new(),
            playlists: Vec::new(),
            view: LibraryView::All,
            current: None,
            playing: false,
            position: 0.0,
            volume: 0.7,
            last_volume: 0.7,
            shuffle: false,
            repeat: RepeatMode::Off,
            sort: None,
            queue: Vec::new(),
            shuffle_order: Vec::new(),
            rng: 0x1234_5678,
            discord_rpc: true,
            auto_video_bg: false,
            shuffle_smart: true,
            playlist_density: PlaylistDensity::Normal,
            notice: None,
            index_by_id: HashMap::new(),
            playlist_of: HashMap::new(),
            search_folds: Vec::new(),
            engine: None,
            rack: AudioRack::flat(),
            armed_at: None,
            heard_to: 0.0,
            hold: false,
            db: None,
        }
    }

    pub fn preview() -> Self {
        let tracks = sample_tracks();
        let playlists = vec![
            Playlist {
                id: "night".into(),
                name: "Night Drive".into(),
                track_ids: (0..7).map(TrackId).collect(),
                cover: None,
                cover_path: None,
                mix_mode: false,
            },
            Playlist {
                id: "focus".into(),
                name: "Focus".into(),
                track_ids: (4..12).map(TrackId).collect(),
                cover: None,
                cover_path: None,
                mix_mode: false,
            },
        ];
        let mut session = Self {
            tracks,
            playlists,
            view: LibraryView::All,
            current: None,
            playing: false,
            position: 0.0,
            volume: 0.7,
            last_volume: 0.7,
            shuffle: false,
            repeat: RepeatMode::Off,
            sort: None,
            queue: Vec::new(),
            shuffle_order: Vec::new(),
            rng: 0x1234_5678,
            discord_rpc: true,
            auto_video_bg: false,
            shuffle_smart: true,
            playlist_density: PlaylistDensity::Normal,
            notice: None,
            index_by_id: HashMap::new(),
            playlist_of: HashMap::new(),
            search_folds: Vec::new(),
            engine: None,
            rack: AudioRack::flat(),
            armed_at: None,
            heard_to: 0.0,
            hold: false,
            db: None,
        };
        session.rebuild_indexes();
        session
    }

    fn rebuild_indexes(&mut self) {
        self.index_by_id = self
            .tracks
            .iter()
            .enumerate()
            .map(|(index, track)| (track.id, index))
            .collect();
        self.playlist_of.clear();
        for playlist in &self.playlists {
            for id in &playlist.track_ids {
                self.playlist_of
                    .entry(*id)
                    .or_insert_with(|| playlist.name.clone());
            }
        }
        let folds_match = self.search_folds.len() == self.tracks.len()
            && self.search_folds.first().map(|fold| fold.id)
                == self.tracks.first().map(|track| track.id);
        if !folds_match {
            self.search_folds = self
                .tracks
                .iter()
                .map(|track| SearchFold {
                    id: track.id,
                    title: track.title.to_lowercase(),
                    artist: track.artist.to_lowercase(),
                    album: track.album.to_lowercase(),
                })
                .collect();
        }
    }

    pub fn track_by_index(&self, index: usize) -> Option<&Track> {
        self.tracks.get(index)
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.index_by_id
            .get(&id)
            .and_then(|index| self.tracks.get(*index))
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub fn liked_count(&self) -> usize {
        self.tracks.iter().filter(|track| track.liked).count()
    }

    pub fn current_track(&self) -> Option<&Track> {
        self.current.and_then(|id| self.track(id))
    }

    pub fn view_label(&self) -> String {
        match &self.view {
            LibraryView::All => "All tracks".into(),
            LibraryView::Liked => "Liked".into(),
            LibraryView::Playlist(id) => self
                .playlists
                .iter()
                .find(|playlist| &playlist.id == id)
                .map(|playlist| playlist.name.clone())
                .unwrap_or_else(|| "Playlist".into()),
        }
    }

    pub fn visible_tracks(&self, query: &str) -> Vec<Track> {
        self.visible_indexes(query)
            .into_iter()
            .filter_map(|index| self.tracks.get(index).cloned())
            .collect()
    }

    pub fn visible_indexes(&self, query: &str) -> Vec<usize> {
        let parsed = parse_query(query);
        let mut indexes: Vec<usize> = match &self.view {
            LibraryView::Playlist(id) => self
                .playlists
                .iter()
                .find(|playlist| &playlist.id == id)
                .map(|playlist| {
                    playlist
                        .track_ids
                        .iter()
                        .filter_map(|id| self.index_by_id.get(id).copied())
                        .filter(|index| {
                            self.tracks.get(*index).is_some_and(|track| {
                                self.matches_playlist_filter(track, &parsed)
                                    && matches_query(track, &parsed)
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => self
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, track)| self.in_view(track, &parsed) && matches_query(track, &parsed))
                .map(|(index, _)| index)
                .collect(),
        };
        if let Some(sort) = self.sort {
            indexes.sort_by(|left, right| {
                let (Some(left), Some(right)) =
                    (self.tracks.get(*left), self.tracks.get(*right))
                else {
                    return std::cmp::Ordering::Equal;
                };
                let ordering = match sort.column {
                    SortColumn::Title => left.title.to_lowercase().cmp(&right.title.to_lowercase()),
                    SortColumn::Album => left.album.to_lowercase().cmp(&right.album.to_lowercase()),
                    SortColumn::Duration => left
                        .duration
                        .partial_cmp(&right.duration)
                        .unwrap_or(std::cmp::Ordering::Equal),
                };
                if sort.descending {
                    ordering.reverse()
                } else {
                    ordering
                }
            });
        }
        indexes
    }

    /// Hit ids for the search dropdown, in library order. Empty when the query
    /// has no text or is a media URL. Unlike [`Self::visible_tracks`], this
    /// ignores the open library view. Does not clone track bodies.
    pub fn search_ids(&self, query: &str) -> Vec<TrackId> {
        if !has_search_text(query) {
            return Vec::new();
        }
        let parsed = parse_query(query);
        let allowed = self.playlist_filter_ids(parsed.playlist.as_deref());
        self.search_folds
            .iter()
            .filter(|fold| allowed.as_ref().is_none_or(|ids| ids.contains(&fold.id)))
            .filter(|fold| fold_matches(fold, &parsed))
            .map(|fold| fold.id)
            .collect()
    }

    /// Track ids that belong to a playlist whose name contains `name`.
    /// `None` means the query has no playlist filter.
    fn playlist_filter_ids(&self, name: Option<&str>) -> Option<HashSet<TrackId>> {
        let name = name?;
        let needle = name.to_lowercase();
        let mut ids = HashSet::new();
        for playlist in &self.playlists {
            if playlist.name.to_lowercase().contains(&needle) {
                ids.extend(playlist.track_ids.iter().copied());
            }
        }
        Some(ids)
    }

    pub fn playlist_label(&self, id: TrackId) -> Option<String> {
        self.playlist_of.get(&id).cloned()
    }

    fn matches_playlist_filter(&self, track: &Track, query: &ParsedQuery) -> bool {
        let Some(name) = &query.playlist else {
            return true;
        };
        let needle = name.to_lowercase();
        self.playlists.iter().any(|playlist| {
            playlist.name.to_lowercase().contains(&needle) && playlist.track_ids.contains(&track.id)
        })
    }

    fn in_view(&self, track: &Track, query: &ParsedQuery) -> bool {
        if !self.matches_playlist_filter(track, query) {
            return false;
        }
        match &self.view {
            LibraryView::All => true,
            LibraryView::Liked => track.liked,
            LibraryView::Playlist(id) => self
                .playlists
                .iter()
                .find(|playlist| &playlist.id == id)
                .is_some_and(|playlist| playlist.track_ids.contains(&track.id)),
        }
    }

    pub fn suggestions(&self, query: &str) -> Vec<Suggestion> {
        let trimmed = query.trim_end();
        let (prefix, token) = match trimmed.rfind(char::is_whitespace) {
            Some(index) => (&trimmed[..=index], trimmed[index + 1..].trim()),
            None => ("", trimmed),
        };
        let token_lower = token.to_lowercase();
        // The filter menu opens only while the current word starts with '@'.
        if !token_lower.starts_with('@') {
            return Vec::new();
        }

        if let Some((modifier, needle)) = playlist_value_prefix(&token_lower) {
            return self
                .playlists
                .iter()
                .filter(|playlist| {
                    needle.is_empty() || playlist.name.to_lowercase().contains(needle)
                })
                .take(6)
                .map(|playlist| Suggestion {
                    label: format!("{modifier}{}", playlist.name).into(),
                    detail: "Playlist".into(),
                    insert: format!("{prefix}{modifier}{} ", playlist.name),
                })
                .collect();
        }

        // Short and long names. Bare `@p` stays in this list so `@playlist` is still offered.
        let modifiers = [
            ("@a", "Search artist", "@a "),
            ("@t", "Search title", "@t "),
            ("@p", "Filter by playlist", "@p="),
            ("@artist", "Search artist", "@artist "),
            ("@title", "Search title", "@title "),
            ("@playlist", "Filter by playlist", "@playlist="),
        ];
        modifiers
            .into_iter()
            .filter(|(label, _, _)| label.starts_with(&token_lower))
            .map(|(label, detail, insert)| Suggestion {
                label: label.into(),
                detail: detail.into(),
                insert: format!("{prefix}{insert}"),
            })
            .collect()
    }

    pub fn toggle_sort(&mut self, column: SortColumn) {
        self.sort = match self.sort {
            Some(sort) if sort.column == column && !sort.descending => Some(Sort {
                column,
                descending: true,
            }),
            Some(sort) if sort.column == column && sort.descending => None,
            _ => Some(Sort {
                column,
                descending: false,
            }),
        };
    }

    pub fn toggle_like(&mut self, id: TrackId) {
        let Some(liked) = self.tracks.iter().find(|track| track.id == id).map(|track| track.liked)
        else {
            return;
        };
        let next = !liked;
        if let Err(error) = self.write_like(id, next) {
            self.notice = Some(error);
            return;
        }
        if let Some(track) = self.tracks.iter_mut().find(|track| track.id == id) {
            track.liked = next;
        }
    }

    fn write_like(&self, id: TrackId, liked: bool) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        if liked {
            db.execute(
                "INSERT OR IGNORE INTO liked_tracks(track_id, position)
                 VALUES (?1, (SELECT COALESCE(MAX(position), -1) + 1 FROM liked_tracks))",
                [id.0 as i64],
            )
            .map_err(|error| format!("Could not save like: {error}"))?;
        } else {
            db.execute("DELETE FROM liked_tracks WHERE track_id = ?1", [id.0 as i64])
                .map_err(|error| format!("Could not remove like: {error}"))?;
        }
        Ok(())
    }

    pub fn create_playlist(&mut self) {
        let n = self.playlists.len() + 1;
        let id = format!("playlist-{n}");
        let name = format!("Playlist {n}");
        if let Err(error) = self.insert_playlist(&id, &name) {
            self.notice = Some(error);
            return;
        }
        self.playlists.push(Playlist {
            id: id.clone(),
            name,
            track_ids: Vec::new(),
            cover: None,
            cover_path: None,
            mix_mode: false,
        });
        self.view = LibraryView::Playlist(id);
        self.rebuild_indexes();
    }

    pub fn rename_playlist(&mut self, id: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Err(error) = self.write_playlist_name(id, name) {
            self.push_notice(error);
            return;
        }
        if let Some(playlist) = self.playlists.iter_mut().find(|playlist| playlist.id == id) {
            playlist.name = name.to_string();
        }
    }

    pub fn delete_playlist(&mut self, id: &str) {
        if let Err(error) = self.delete_playlist_row(id) {
            self.push_notice(error);
            return;
        }
        self.playlists.retain(|playlist| playlist.id != id);
        if matches!(&self.view, LibraryView::Playlist(open) if open == id) {
            self.view = LibraryView::All;
        }
        self.rebuild_indexes();
    }

    /// Copies one library track onto a playlist. Already-present tracks stay put.
    pub fn add_to_playlist(&mut self, playlist_id: &str, track: TrackId) -> bool {
        let Some(playlist) = self.playlists.iter().find(|playlist| playlist.id == playlist_id)
        else {
            return false;
        };
        if playlist.track_ids.contains(&track) {
            return false;
        }
        if let Err(error) = self.insert_playlist_track(playlist_id, track) {
            self.push_notice(error);
            return false;
        }
        if let Some(playlist) = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == playlist_id)
        {
            playlist.track_ids.push(track);
        }
        self.rebuild_indexes();
        true
    }

    /// Removes a track from a playlist. The file stays in the library.
    pub fn remove_from_playlist(&mut self, playlist_id: &str, track: TrackId) {
        let Some(playlist) = self.playlists.iter().find(|playlist| playlist.id == playlist_id)
        else {
            return;
        };
        if !playlist.track_ids.contains(&track) {
            return;
        }
        if let Err(error) = self.delete_playlist_track(playlist_id, track) {
            self.push_notice(error);
            return;
        }
        if let Some(playlist) = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == playlist_id)
        {
            playlist.track_ids.retain(|id| *id != track);
        }
        self.rebuild_indexes();
    }

    pub fn set_mix_mode(&mut self, id: &str, enabled: bool) {
        if let Err(error) = self.write_mix_mode(id, enabled) {
            self.push_notice(error);
            return;
        }
        if let Some(playlist) = self.playlists.iter_mut().find(|playlist| playlist.id == id) {
            playlist.mix_mode = enabled;
        }
    }

    pub fn set_playlist_cover(&mut self, id: &str, source: &std::path::Path) {
        let path = match crate::ui::catalog::store_playlist_cover(id, source) {
            Ok(path) => path,
            Err(error) => {
                self.push_notice(error);
                return;
            }
        };
        let text = path.to_string_lossy().to_string();
        if let Err(error) = self.write_cover_path(id, Some(&text)) {
            self.push_notice(error);
            return;
        }
        if let Some(playlist) = self.playlists.iter_mut().find(|playlist| playlist.id == id) {
            playlist.cover = Some(path);
            playlist.cover_path = Some(text);
        }
    }

    pub fn clear_playlist_cover(&mut self, id: &str) {
        crate::ui::catalog::remove_playlist_cover_files(id);
        if let Err(error) = self.write_cover_path(id, None) {
            self.push_notice(error);
            return;
        }
        let ids = self
            .playlists
            .iter()
            .find(|playlist| playlist.id == id)
            .map(|playlist| playlist.track_ids.clone())
            .unwrap_or_default();
        let fallback = ids
            .iter()
            .find_map(|track| self.track(*track).and_then(|track| track.cover.clone()));
        if let Some(playlist) = self.playlists.iter_mut().find(|playlist| playlist.id == id) {
            playlist.cover_path = None;
            playlist.cover = fallback;
        }
    }

    fn write_playlist_name(&self, id: &str, name: &str) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        db.execute(
            "UPDATE playlists SET name = ?2 WHERE id = ?1",
            rusqlite::params![id, name],
        )
        .map_err(|error| format!("Could not rename playlist: {error}"))?;
        Ok(())
    }

    fn delete_playlist_row(&self, id: &str) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        db.execute("DELETE FROM playlists WHERE id = ?1", [id])
            .map_err(|error| format!("Could not delete playlist: {error}"))?;
        let ids = {
            let mut statement = db
                .prepare("SELECT id FROM playlists ORDER BY position, id")
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        for (position, playlist_id) in ids.iter().enumerate() {
            db.execute(
                "UPDATE playlists SET position = ?2 WHERE id = ?1",
                rusqlite::params![playlist_id, position as i64],
            )
            .map_err(|error| format!("Could not delete playlist: {error}"))?;
        }
        Ok(())
    }

    fn insert_playlist_track(&self, playlist_id: &str, track: TrackId) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        db.execute(
            "INSERT OR IGNORE INTO playlist_tracks(playlist_id, track_id, position)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(position), -1) + 1
                                FROM playlist_tracks WHERE playlist_id = ?1))",
            rusqlite::params![playlist_id, track.0 as i64],
        )
        .map_err(|error| format!("Could not add to playlist: {error}"))?;
        Ok(())
    }

    fn delete_playlist_track(&self, playlist_id: &str, track: TrackId) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        db.execute(
            "DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND track_id = ?2",
            rusqlite::params![playlist_id, track.0 as i64],
        )
        .map_err(|error| format!("Could not remove the track: {error}"))?;
        let ids = {
            let mut statement = db
                .prepare(
                    "SELECT track_id FROM playlist_tracks
                      WHERE playlist_id = ?1
                      ORDER BY position, track_id",
                )
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([playlist_id], |row| row.get::<_, i64>(0))
                .map_err(|error| error.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        };
        for (position, track_id) in ids.iter().enumerate() {
            db.execute(
                "UPDATE playlist_tracks SET position = ?3
                  WHERE playlist_id = ?1 AND track_id = ?2",
                rusqlite::params![playlist_id, track_id, position as i64],
            )
            .map_err(|error| format!("Could not remove the track: {error}"))?;
        }
        Ok(())
    }

    fn write_mix_mode(&self, id: &str, enabled: bool) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        db.execute(
            "UPDATE playlists SET mix_mode = ?2 WHERE id = ?1",
            rusqlite::params![id, enabled as i64],
        )
        .map_err(|error| format!("Could not set mix mode: {error}"))?;
        Ok(())
    }

    fn write_cover_path(&self, id: &str, path: Option<&str>) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        db.execute(
            "UPDATE playlists SET cover_path = ?2 WHERE id = ?1",
            rusqlite::params![id, path],
        )
        .map_err(|error| format!("Could not save the cover: {error}"))?;
        Ok(())
    }

    fn insert_playlist(&self, id: &str, name: &str) -> Result<(), String> {
        let Some(db) = &self.db else {
            return Ok(());
        };
        db.execute(
            "INSERT INTO playlists(id, name, position)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(position), -1) + 1 FROM playlists))",
            rusqlite::params![id, name],
        )
        .map_err(|error| format!("Could not save playlist: {error}"))?;
        Ok(())
    }

    pub fn play(&mut self, id: TrackId, queue: Vec<TrackId>) {
        self.queue = if queue.iter().any(|queued| *queued == id) {
            queue
        } else {
            let mut queue = queue;
            queue.insert(0, id);
            queue
        };
        self.current = Some(id);
        self.position = 0.0;
        self.playing = true;
        if self.shuffle {
            self.rebuild_shuffle();
        }
        self.output_play(0.0);
    }

    /// Shuffle applies to the song that is already open, not only the next play.
    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        if self.shuffle {
            self.rebuild_shuffle();
        } else {
            self.shuffle_order.clear();
        }
        self.refresh_gapless();
    }

    /// Repeat applies to the song that is already open.
    pub fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.next();
        self.refresh_gapless();
    }

    fn rebuild_shuffle(&mut self) {
        let mut order = self.queue.clone();
        for index in (1..order.len()).rev() {
            let swap = (self.rand() as usize) % (index + 1);
            order.swap(index, swap);
        }
        if let Some(current) = self.current {
            if let Some(pos) = order.iter().position(|id| *id == current) {
                order.swap(0, pos);
            }
        }
        self.shuffle_order = order;
    }

    pub fn toggle_playback(&mut self, queue: Vec<TrackId>) {
        if self.current.is_some() {
            self.playing = !self.playing;
            if self.playing {
                if self.output_holds_current() {
                    self.output_resume();
                } else {
                    let at = self.position;
                    self.output_play(at);
                }
            } else {
                self.output_pause();
            }
            return;
        }
        if let Some(first) = queue.first().copied() {
            self.play(first, queue);
        }
    }

    pub fn next(&mut self) {
        self.skip(1);
    }

    pub fn prev(&mut self) {
        if self.position > 2.0 {
            self.position = 0.0;
            self.playing = true;
            if self.output_holds_current() {
                self.output_seek(0.0);
                self.output_resume();
            } else {
                self.output_play(0.0);
            }
            return;
        }
        self.skip(-1);
    }

    fn skip(&mut self, direction: i32) {
        let Some(current) = self.current else {
            if let Some(first) = self.queue.first().copied() {
                self.play(first, self.queue.clone());
            }
            return;
        };
        if direction > 0 && self.repeat == RepeatMode::One {
            self.position = 0.0;
            self.playing = true;
            self.output_play(0.0);
            return;
        }
        if self.queue.is_empty() {
            self.playing = false;
            self.output_pause();
            return;
        }
        if self.shuffle
            && direction > 0
            && !self.shuffle_order.iter().any(|queued| *queued == current)
        {
            self.rebuild_shuffle();
        }
        let repeat_all = self.repeat == RepeatMode::All;
        let next = if self.shuffle && direction > 0 {
            step_forward(&self.shuffle_order, current, repeat_all)
        } else if direction < 0 {
            let wrap = !self.shuffle && repeat_all;
            let order = if self.shuffle {
                self.shuffle_order.as_slice()
            } else {
                self.queue.as_slice()
            };
            step_back(order, current, wrap)
        } else {
            step_forward(&self.queue, current, repeat_all)
        };
        if let Some(id) = next {
            self.current = Some(id);
            self.position = 0.0;
            self.playing = true;
            self.output_play(0.0);
        } else {
            self.playing = false;
            if let Some(track) = self.track(current) {
                self.position = track.duration;
            }
            self.output_pause();
        }
    }

    pub fn seek(&mut self, fraction: f32) {
        let Some(track) = self.current_track() else {
            return;
        };
        self.position = (fraction as f64).clamp(0.0, 1.0) * track.duration;
        self.heard_to = self.position;
        self.output_seek(self.position);
    }

    /// Move the playhead by a number of seconds, clamped to the current track.
    pub fn seek_by(&mut self, seconds: f64) {
        let Some(track) = self.current_track() else {
            return;
        };
        if track.duration <= 0.0 {
            return;
        }
        self.position = (self.position + seconds).clamp(0.0, track.duration);
        self.heard_to = self.position;
        self.output_seek(self.position);
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if self.volume > 0.0 {
            self.last_volume = self.volume;
        }
        self.output_volume();
    }

    pub fn toggle_mute(&mut self) {
        if self.volume > 0.0 {
            self.last_volume = self.volume;
            self.volume = 0.0;
        } else {
            self.volume = if self.last_volume > 0.0 {
                self.last_volume
            } else {
                0.7
            };
        }
        self.output_volume();
    }

    /// Advances playback. With BASS attached, the position comes from the
    /// engine. Without it, this is the preview clock used by tests.
    pub fn tick(&mut self, dt: f64) -> bool {
        if self.engine.is_some() {
            return self.poll_output();
        }
        if !self.playing {
            return false;
        }
        let Some(duration) = self.current_track().map(|track| track.duration) else {
            self.playing = false;
            return true;
        };
        self.position += dt;
        if self.position >= duration {
            let mode = self.repeat;
            if mode == RepeatMode::One {
                self.position = 0.0;
            } else {
                self.next();
            }
        }
        true
    }

    fn poll_output(&mut self) -> bool {
        if !self.playing && self.armed_at.is_none() {
            return false;
        }
        let snap = {
            let Some(engine) = self.engine.as_ref() else {
                return false;
            };
            engine.snapshot()
        };
        if matches!(
            snap.state,
            PlaybackState::Playing | PlaybackState::Paused | PlaybackState::Stalled
        ) {
            let mut changed = (self.position - snap.position).abs() > 0.01;
            self.position = snap.position;
            if let Some(path) = snap.current_file.as_deref() {
                if let Some(id) = self.track_id_for_path(path) {
                    if self.current != Some(id) {
                        self.current = Some(id);
                        self.heard_to = snap.position;
                        self.refresh_gapless();
                        changed = true;
                    }
                }
            }
            if snap.position > self.heard_to {
                self.heard_to = snap.position;
            }
            let duration = snap.duration.max(
                self.current_track()
                    .map(|track| track.duration)
                    .unwrap_or(0.0),
            );
            match snap.state {
                PlaybackState::Playing if !self.hold => self.playing = true,
                // A paused frame no longer blocks the following stop from
                // advancing. Pause itself stays off until the user resumes.
                PlaybackState::Paused if output_settled(self.armed_at, duration) => {
                    self.playing = false;
                }
                _ => {}
            }
            return changed;
        }
        // Stopped right after open, or for a glitch in the middle, is not the
        // end. A decoder that rewinds to zero at the real end still advances.
        if let Some(path) = snap.current_file.as_deref() {
            if let Some(id) = self.track_id_for_path(path) {
                if self.current != Some(id) {
                    self.current = Some(id);
                    self.heard_to = snap.position;
                    self.position = snap.position;
                }
            }
        }
        let duration = snap.duration.max(
            self.current_track()
                .map(|track| track.duration)
                .unwrap_or(0.0),
        );
        if self.hold || !output_settled(self.armed_at, duration) {
            return false;
        }
        if playback_reached_end(self.heard_to, snap.position, duration) {
            self.next();
            return true;
        }
        self.position = snap.position;
        false
    }

    fn output_play(&mut self, resume_at: f64) {
        if self.engine.is_none() {
            return;
        }
        let Some(id) = self.current else {
            return;
        };
        let Some(path) = self.track(id).map(|track| track.path.clone()) else {
            return;
        };
        let queue = self.output_queue(id);
        let volume = self.volume;
        self.armed_at = Some(Instant::now());
        self.heard_to = resume_at;
        self.hold = false;
        let played = self
            .engine
            .as_ref()
            .expect("engine")
            .play(&path, queue);
        if let Err(error) = played {
            self.playing = false;
            self.hold = true;
            self.push_notice(error);
            return;
        }
        if resume_at > 0.25 {
            if let Some(engine) = self.engine.as_ref() {
                let _ = engine.seek(resume_at);
            }
            self.position = resume_at;
        }
        if let Some(engine) = self.engine.as_ref() {
            let _ = engine.set_volume(volume);
        }
    }

    fn output_pause(&mut self) {
        self.hold = true;
        if let Some(engine) = &self.engine {
            let _ = engine.pause();
        }
    }

    fn output_resume(&mut self) {
        let volume = self.volume;
        self.hold = false;
        self.armed_at = Some(Instant::now());
        if let Some(engine) = &self.engine {
            let _ = engine.resume();
            let _ = engine.set_volume(volume);
        }
    }

    fn output_seek(&mut self, seconds: f64) {
        if let Some(engine) = &self.engine {
            let _ = engine.seek(seconds);
        }
    }

    fn output_volume(&mut self) {
        if let Some(engine) = &self.engine {
            let _ = engine.set_volume(self.volume);
        }
    }

    fn output_holds_current(&self) -> bool {
        let Some(engine) = &self.engine else {
            return false;
        };
        let Some(path) = self.current_track().map(|track| track.path.clone()) else {
            return false;
        };
        engine.holds(&path)
    }

    fn refresh_gapless(&self) {
        if self.engine.is_none() {
            return;
        }
        let Some(id) = self.current else {
            return;
        };
        let Some(path) = self.track(id).map(|track| track.path.clone()) else {
            return;
        };
        let queue = self.output_queue(id);
        if let Some(engine) = &self.engine {
            engine.prepare(&path, queue);
        }
    }

    /// Current track plus the next few, so BASS can open the follower early.
    /// Repeat-one stays on this file. Shuffle follows `shuffle_order`.
    fn output_queue(&self, id: TrackId) -> Vec<GaplessTrack> {
        let ids = self.upcoming_ids(id);
        let mut queue = Vec::new();
        for queued in ids {
            let Some(path) = self.track(queued).map(|track| track.path.clone()) else {
                continue;
            };
            queue.push(playback::gapless_track(&path));
        }
        if queue.is_empty() {
            if let Some(path) = self.track(id).map(|track| track.path.clone()) {
                queue.push(playback::gapless_track(&path));
            }
        }
        queue
    }

    pub(crate) fn upcoming_ids(&self, id: TrackId) -> Vec<TrackId> {
        if self.repeat == RepeatMode::One || self.queue.is_empty() {
            return vec![id];
        }
        let repeat_all = self.repeat == RepeatMode::All;
        if self.shuffle {
            gapless_window(&self.shuffle_order, id, repeat_all)
        } else {
            gapless_window(&self.queue, id, repeat_all)
        }
    }

    fn track_id_for_path(&self, path: &str) -> Option<TrackId> {
        self.queue.iter().copied().find(|id| {
            self.track(*id)
                .is_some_and(|track| playback::same_audio_path(&track.path, path))
        }).or_else(|| {
            self.tracks.iter().find_map(|track| {
                playback::same_audio_path(&track.path, path).then_some(track.id)
            })
        })
    }

    pub fn progress(&self) -> f32 {
        let Some(track) = self.current_track() else {
            return 0.0;
        };
        if track.duration <= 0.0 {
            return 0.0;
        }
        (self.position / track.duration).clamp(0.0, 1.0) as f32
    }

    fn rand(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        if x == 0 {
            x = 0xA5A5_A5A5;
        }
        self.rng = x;
        x
    }
}

struct ParsedQuery {
    field: SearchField,
    playlist: Option<String>,
    text: String,
}

/// `@p=` or `@playlist=` plus the playlist name typed after the equals sign.
fn playlist_value_prefix(token: &str) -> Option<(&str, &str)> {
    if let Some(name) = token.strip_prefix("@playlist=") {
        Some(("@playlist=", name.trim()))
    } else if let Some(name) = token.strip_prefix("@p=") {
        Some(("@p=", name.trim()))
    } else {
        None
    }
}

fn parse_query(raw: &str) -> ParsedQuery {
    let mut field = SearchField::Both;
    let mut playlist = None;
    let mut text = Vec::new();
    for token in raw.split_whitespace() {
        let lower = token.to_lowercase();
        match lower.as_str() {
            "@artist" | "@a" => field = SearchField::Artist,
            "@title" | "@t" => field = SearchField::Title,
            "@playlist" | "@p" => {}
            _ => match playlist_value_prefix(&lower) {
                Some((modifier, _)) => {
                    playlist = Some(token[modifier.len()..].trim().to_string());
                }
                // `@`, `@art`, `@play` are a filter still being typed, not search text.
                None if lower.starts_with('@') => {}
                None => text.push(token.to_lowercase()),
            },
        }
    }
    ParsedQuery {
        field,
        playlist: playlist.filter(|name| !name.is_empty()),
        text: text.join(" "),
    }
}

fn matches_query(track: &Track, query: &ParsedQuery) -> bool {
    if query.text.is_empty() {
        return true;
    }
    let title = track.title.to_lowercase();
    let artist = track.artist.to_lowercase();
    match query.field {
        SearchField::Title => title.contains(&query.text),
        SearchField::Artist => artist.contains(&query.text),
        SearchField::Both => {
            title.contains(&query.text)
                || artist.contains(&query.text)
                || track.album.to_lowercase().contains(&query.text)
        }
    }
}

/// Same rules as [`matches_query`], on the folded strings from load time.
fn fold_matches(fold: &SearchFold, query: &ParsedQuery) -> bool {
    if query.text.is_empty() {
        return true;
    }
    match query.field {
        SearchField::Title => fold.title.contains(&query.text),
        SearchField::Artist => fold.artist.contains(&query.text),
        SearchField::Both => {
            fold.title.contains(&query.text)
                || fold.artist.contains(&query.text)
                || fold.album.contains(&query.text)
        }
    }
}

fn step_forward(order: &[TrackId], id: TrackId, repeat_all: bool) -> Option<TrackId> {
    if order.is_empty() {
        return None;
    }
    let pos = order.iter().position(|queued| *queued == id).unwrap_or(0);
    if pos + 1 < order.len() {
        Some(order[pos + 1])
    } else if repeat_all {
        order.first().copied()
    } else {
        None
    }
}

fn step_back(order: &[TrackId], id: TrackId, wrap: bool) -> Option<TrackId> {
    if order.is_empty() {
        return None;
    }
    let pos = order.iter().position(|queued| *queued == id).unwrap_or(0);
    if pos > 0 {
        Some(order[pos - 1])
    } else if wrap {
        order.last().copied()
    } else {
        None
    }
}

/// Current id plus up to three followers. A one-track repeat does not preload itself.
fn gapless_window(order: &[TrackId], id: TrackId, repeat_all: bool) -> Vec<TrackId> {
    if order.is_empty() {
        return vec![id];
    }
    let Some(start) = order.iter().position(|queued| *queued == id) else {
        return vec![id];
    };
    let mut out = Vec::new();
    for step in 0..4 {
        let index = start + step;
        let queued = if index < order.len() {
            order[index]
        } else if repeat_all {
            order[index % order.len()]
        } else {
            break;
        };
        if out.last() == Some(&queued) {
            break;
        }
        out.push(queued);
    }
    if out.is_empty() {
        out.push(id);
    }
    out
}

/// True when a stopped stream is the end of the file, including a rewind to zero.
/// One Alt+wheel notch, as a fraction of full volume. Positive is louder.
/// A notch is one event: Windows reports several lines, and that still counts as 1%.
pub(crate) fn volume_wheel_step(delta_y: f32) -> Option<f32> {
    if delta_y > 0.0 {
        Some(0.01)
    } else if delta_y < 0.0 {
        Some(-0.01)
    } else {
        None
    }
}

pub(crate) fn playback_reached_end(heard: f64, position: f64, duration: f64) -> bool {
    if !duration.is_finite() || duration <= 0.0 {
        return false;
    }
    if position + 0.4 >= duration {
        return true;
    }
    heard + 0.75 >= duration && position < 1.5
}

/// Long tracks ignore a stop in the first 1.5s. Short tracks may end sooner.
fn output_settled(armed_at: Option<Instant>, duration: f64) -> bool {
    let Some(at) = armed_at else {
        return true;
    };
    let guard_ms = if duration.is_finite() && duration > 0.0 {
        (duration * 1000.0 - 80.0).clamp(120.0, 1500.0)
    } else {
        1500.0
    };
    at.elapsed().as_secs_f64() * 1000.0 >= guard_ms
}

pub fn format_time(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0:00".into();
    }
    let total = secs.floor() as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub fn looks_like_url(query: &str) -> bool {
    let value = query.trim();
    value.starts_with("http://")
        || value.starts_with("https://")
        || value.contains("youtube.com")
        || value.contains("youtu.be")
}

/// A query that should open the results plate: some search text, and not a URL.
/// `@artist` or `@p=Focus` alone have no text, so the plate stays hidden.
pub fn has_search_text(query: &str) -> bool {
    !looks_like_url(query) && !parse_query(query).text.is_empty()
}

pub fn describe_search(query: &str) -> String {
    let parsed = parse_query(query);
    let field = match parsed.field {
        SearchField::Artist => "artist",
        SearchField::Title => "title",
        SearchField::Both => "title or artist",
    };
    match parsed.playlist {
        Some(name) => format!("{field} · in \"{name}\""),
        None => format!("{field} · all playlists"),
    }
}

fn sample_tracks() -> Vec<Track> {
    let rows = [
        (
            "Glass Elevator",
            "Night Library",
            "After Hours",
            214.0,
            true,
        ),
        ("Low Voltage", "Night Library", "After Hours", 186.0, false),
        ("Satellite Bloom", "Ada North", "Soft Cities", 241.0, true),
        ("Paper Boats", "Ada North", "Soft Cities", 198.0, false),
        ("Second Desk", "Room Tone", "Focus", 312.0, false),
        ("Quiet Arithmetic", "Room Tone", "Focus", 276.0, true),
        ("Harbor Loop", "Mira Chen", "Coastline", 205.0, false),
        ("Tin Roof Radio", "Mira Chen", "Coastline", 173.0, false),
        ("Blue Pencil", "The Late Set", "Studio B", 254.0, true),
        ("Window Seat", "The Late Set", "Studio B", 221.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Marble Static", "Kite Museum", "Field Notes", 289.0, false),
        ("Last Tram", "Kite Museum", "Field Notes", 247.0, true),
    ];
    rows.into_iter()
        .enumerate()
        .map(|(index, (title, artist, album, duration, liked))| Track {
            id: TrackId(index as u32),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            duration,
            liked,
            path: String::new(),
            cover: None,
            lyrics: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_filter_menu_lists_short_and_long_names() {
        let session = Session::preview();
        assert!(session.suggestions("").is_empty());
        assert!(session.suggestions("ada").is_empty());

        let labels: Vec<_> = session
            .suggestions("@")
            .into_iter()
            .map(|suggestion| suggestion.label.to_string())
            .collect();
        assert_eq!(
            labels,
            vec!["@a", "@t", "@p", "@artist", "@title", "@playlist"]
        );

        let partial: Vec<_> = session
            .suggestions("@p")
            .into_iter()
            .map(|suggestion| suggestion.label.to_string())
            .collect();
        assert_eq!(partial, vec!["@p", "@playlist"]);

        let named: Vec<_> = session
            .suggestions("@playlist=fo")
            .into_iter()
            .map(|suggestion| suggestion.insert)
            .collect();
        assert_eq!(named, vec!["@playlist=Focus "]);
    }

    #[test]
    fn playlist_long_modifier_limits_tracks() {
        let session = Session::preview();
        let tracks = session.visible_tracks("@playlist=Focus");
        assert!(tracks.iter().any(|track| track.title == "Second Desk"));
        assert!(tracks.iter().all(|track| {
            session
                .playlists
                .iter()
                .find(|playlist| playlist.name == "Focus")
                .unwrap()
                .track_ids
                .contains(&track.id)
        }));
    }

    #[test]
    fn search_filters_by_artist_modifier() {
        let session = Session::preview();
        let tracks = session.visible_tracks("@artist ada");
        assert!(tracks.iter().all(|track| track.artist.contains("Ada")));
        assert_eq!(tracks.len(), 2);
    }

    #[test]
    fn liked_view_only_returns_liked_tracks() {
        let mut session = Session::preview();
        session.view = LibraryView::Liked;
        let tracks = session.visible_tracks("");
        assert!(tracks.iter().all(|track| track.liked));
        assert_eq!(tracks.len(), session.liked_count());
    }

    fn search_titles(session: &Session, query: &str) -> Vec<String> {
        session
            .search_ids(query)
            .into_iter()
            .filter_map(|id| session.track(id).map(|track| track.title.clone()))
            .collect()
    }

    #[test]
    fn search_tracks_ignore_the_open_view() {
        let mut session = Session::preview();
        session.view = LibraryView::Liked;
        let titles = search_titles(&session, "harbor");
        assert!(titles.iter().any(|title| title == "Harbor Loop"));
        assert!(session.visible_tracks("").iter().all(|track| track.liked));
    }

    #[test]
    fn search_tracks_need_text() {
        let session = Session::preview();
        assert!(session.search_ids("@artist").is_empty());
        assert!(session.search_ids("@").is_empty());
        assert!(!has_search_text("@"));
        assert!(session.search_ids("@p=Focus").is_empty());
        assert!(session.search_ids("https://youtu.be/abc").is_empty());
        assert!(search_titles(&session, "@title desk")
            .iter()
            .any(|title| title == "Second Desk"));
        assert_eq!(
            search_titles(&session, "@title desk @p=Focus"),
            vec!["Second Desk".to_string()]
        );
    }

    #[test]
    fn describe_search_names_field_and_scope() {
        assert_eq!(describe_search("ada"), "title or artist · all playlists");
        assert_eq!(describe_search("@artist ada"), "artist · all playlists");
        assert_eq!(
            describe_search("@title desk @p=Focus"),
            "title · in \"Focus\""
        );
    }

    #[test]
    fn playlist_filter_limits_tracks() {
        let session = Session::preview();
        let tracks = session.visible_tracks("@p=Focus");
        assert!(tracks.iter().any(|track| track.title == "Second Desk"));
        assert!(tracks.iter().all(|track| {
            session
                .playlists
                .iter()
                .find(|playlist| playlist.name == "Focus")
                .unwrap()
                .track_ids
                .contains(&track.id)
        }));
    }

    #[test]
    fn alt_wheel_changes_volume_by_one_percent() {
        assert_eq!(volume_wheel_step(3.0), Some(0.01));
        assert_eq!(volume_wheel_step(-0.5), Some(-0.01));
        assert_eq!(volume_wheel_step(0.0), None);
        let mut session = Session::preview();
        session.set_volume(0.5);
        session.set_volume(session.volume + volume_wheel_step(1.0).unwrap());
        assert!((session.volume - 0.51).abs() < 1e-5);
        session.set_volume(1.0);
        session.set_volume(session.volume + 0.01);
        assert!((session.volume - 1.0).abs() < 1e-5);
        session.set_volume(-0.2);
        assert_eq!(session.volume, 0.0);
    }

    #[test]
    fn wheel_seek_steps_five_seconds_and_clamps() {
        let mut session = Session::preview();
        session.play(TrackId(0), vec![TrackId(0)]);
        session.position = 10.0;
        session.seek_by(5.0);
        assert!((session.position - 15.0).abs() < 0.001);
        session.seek_by(-5.0);
        assert!((session.position - 10.0).abs() < 0.001);
        session.seek_by(-100.0);
        assert_eq!(session.position, 0.0);
        session.position = 210.0;
        session.seek_by(5.0);
        assert!((session.position - 214.0).abs() < 0.001);
    }

    #[test]
    fn playback_advances_and_wraps_when_repeat_all() {
        let mut session = Session::preview();
        let queue = vec![TrackId(0), TrackId(1)];
        session.repeat = RepeatMode::All;
        session.play(TrackId(0), queue);
        session.position = 213.5;
        assert!(session.tick(1.0));
        assert_eq!(session.current, Some(TrackId(1)));
        assert!(session.playing);
    }

    #[test]
    fn playlist_menu_edits_the_open_library() {
        let mut session = Session::preview();
        let added = session.add_to_playlist("focus", TrackId(0));
        assert!(added);
        assert!(!session.add_to_playlist("focus", TrackId(0)));
        let focus = session
            .playlists
            .iter()
            .find(|playlist| playlist.id == "focus")
            .unwrap();
        assert!(focus.track_ids.contains(&TrackId(0)));

        session.rename_playlist("night", "  Late  ");
        assert_eq!(
            session
                .playlists
                .iter()
                .find(|playlist| playlist.id == "night")
                .unwrap()
                .name,
            "Late"
        );
        session.rename_playlist("night", "   ");
        assert_eq!(
            session
                .playlists
                .iter()
                .find(|playlist| playlist.id == "night")
                .unwrap()
                .name,
            "Late"
        );

        session.set_mix_mode("night", true);
        assert!(
            session
                .playlists
                .iter()
                .find(|playlist| playlist.id == "night")
                .unwrap()
                .mix_mode
        );

        session.view = LibraryView::Playlist("focus".into());
        session.remove_from_playlist("night", TrackId(0));
        assert!(
            !session
                .playlists
                .iter()
                .find(|playlist| playlist.id == "night")
                .unwrap()
                .track_ids
                .contains(&TrackId(0))
        );
        session.delete_playlist("focus");
        assert!(session.playlists.iter().all(|playlist| playlist.id != "focus"));
        assert_eq!(session.view, LibraryView::All);
    }

    #[test]
    fn stopped_at_zero_counts_as_the_end_only_after_the_song_was_heard() {
        assert!(playback_reached_end(200.0, 0.0, 200.5));
        assert!(playback_reached_end(200.2, 200.3, 200.5));
        assert!(!playback_reached_end(20.0, 0.0, 200.0));
        assert!(!playback_reached_end(0.0, 0.0, 200.0));
        assert!(!playback_reached_end(10.0, 10.0, 0.0));
    }

    #[test]
    fn shuffle_and_repeat_rebuild_the_upcoming_tracks() {
        let mut session = Session::preview();
        let queue: Vec<_> = (0..6).map(TrackId).collect();
        session.play(TrackId(2), queue);
        session.toggle_shuffle();
        assert!(session.shuffle);
        let upcoming = session.upcoming_ids(TrackId(2));
        assert_eq!(upcoming[0], TrackId(2));
        assert!(upcoming.len() > 1);
        session.next();
        assert_eq!(session.current, Some(upcoming[1]));
        assert!(session.playing);

        session.cycle_repeat();
        assert_eq!(session.repeat, RepeatMode::All);
        session.cycle_repeat();
        assert_eq!(session.repeat, RepeatMode::One);
        assert_eq!(session.upcoming_ids(TrackId(2)), vec![TrackId(2)]);

        session.cycle_repeat();
        assert_eq!(session.repeat, RepeatMode::Off);
        session.toggle_shuffle();
        assert!(!session.shuffle);
        assert_eq!(
            session.upcoming_ids(TrackId(2)),
            vec![TrackId(2), TrackId(3), TrackId(4), TrackId(5)]
        );
    }
}
