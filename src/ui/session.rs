// UI session for the main window.
//
// The track list is the library in `%APPDATA%\com.nnfz.muzeeka`. `Session::open`
// plays through BASS. `preview` and `empty` keep the clock so tests stay quiet.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use muzeeka::audio::player::{GaplessTrack, PlaybackState};
use muzeeka::MusicFile;

use gpui::SharedString;
use rusqlite::Connection;

use crate::ui::catalog::{self, LoadedLibrary};
use crate::ui::import::{self, ImportKind};
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

/// How tracks are chosen once the shuffle button is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderMode {
    /// One shuffled copy of the queue. Previous and next walk that copy.
    Shuffle,
    /// Every previous and next picks a track. Nothing is remembered.
    Random,
}

impl OrderMode {
    pub fn detail(self) -> &'static str {
        match self {
            Self::Shuffle => "Shuffle the playlist once. Previous and next follow that order",
            Self::Random => "Previous and next each play a random track",
        }
    }

    /// `settings.json` still stores the old smart/normal names. Both mean shuffle.
    pub fn from_setting(value: Option<&str>) -> Self {
        match value {
            Some("random") => Self::Random,
            _ => Self::Shuffle,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Shuffle => "shuffle",
            Self::Random => "random",
        }
    }

    pub fn load() -> Self {
        let path = catalog::app_data_dir().join("settings.json");
        let Ok(raw) = fs::read_to_string(path) else {
            return Self::Shuffle;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Self::Shuffle;
        };
        Self::from_setting(value.get("shuffle_mode").and_then(|item| item.as_str()))
    }

    pub fn store(self) {
        let path = catalog::app_data_dir().join("settings.json");
        let raw = fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
        let Ok(next) =
            crate::ui::ytdlp::merge_setting(&raw, "shuffle_mode", Some(self.as_setting()))
        else {
            return;
        };
        if next != raw {
            let _ = fs::write(path, next);
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
    /// Liked view order. Independent of library order, so a like drag does not
    /// reshuffle All tracks.
    liked_order: Vec<TrackId>,
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
    /// Shuffled copy of `queue`. Only [`OrderMode::Shuffle`] walks it.
    shuffle_order: Vec<TrackId>,
    rng: u32,
    pub discord_rpc: bool,
    pub auto_video_bg: bool,
    pub order_mode: OrderMode,
    pub playlist_density: PlaylistDensity,
    /// Failed to open the current file, or save the library. Shown beside search.
    pub playback_error: Option<String>,
    playback_error_until: Option<Instant>,
    /// Full audio files that also have `#cue:` rows. Hidden from search and All.
    cue_containers: HashSet<TrackId>,
    /// Bumped when the visible track set changes. Playback position does not.
    list_epoch: u64,
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
    /// Playlist the queue was started from. Empty for All tracks and Liked.
    playing_playlist: Option<String>,
    /// Last successful write of `app_state`. Position updates are throttled.
    playback_saved_at: Option<Instant>,
    playback_stamp: Option<PlaybackStamp>,
    db: Option<Connection>,
}

#[derive(Clone, PartialEq, Eq)]
struct PlaybackStamp {
    file: Option<String>,
    second: i64,
    playing: bool,
}

struct SearchFold {
    id: TrackId,
    title: String,
    artist: String,
    album: String,
}

impl Session {
    /// Real library from the shared app-data folder. A missing database is
    /// created empty. Sample tracks stay in [`Self::preview`] for tests.
    pub fn open() -> Self {
        let mut session = match catalog::load() {
            Ok(library) => Self::from_library(library),
            Err(error) => {
                let mut session = Self::empty();
                session.push_notice(error);
                session
            }
        };
        session.order_mode = OrderMode::load();
        if session.random_jumps() {
            session.shuffle_order.clear();
        }
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
                if session.playing {
                    let at = session.position;
                    session.output_play(at);
                }
            }
            Err(error) => {
                session.playing = false;
                session.push_notice(format!("Audio output is unavailable: {error}"));
            }
        }
        session
    }

    /// Appends a line to the header error, beside search.
    pub(crate) fn push_notice(&mut self, line: String) {
        let line = match self.playback_error.take() {
            Some(existing) => format!("{existing} {line}"),
            None => line,
        };
        self.note_playback_error(line);
    }

    /// Replace the header playback error. It expires on its own, and a later
    /// successful open clears it immediately.
    pub(crate) fn note_playback_error(&mut self, line: String) {
        self.playback_error = Some(line);
        self.playback_error_until = Some(Instant::now() + PLAYBACK_ERROR_TTL);
    }

    pub(crate) fn clear_playback_error(&mut self) {
        self.playback_error = None;
        self.playback_error_until = None;
    }

    fn expire_playback_error(&mut self) -> bool {
        let expired = self
            .playback_error_until
            .is_some_and(|until| Instant::now() >= until);
        if expired {
            self.clear_playback_error();
        }
        expired
    }

    fn from_library(library: LoadedLibrary) -> Self {
        let volume = library.volume;
        let liked_order = catalog::load_liked_order(&library.db).unwrap_or_default();
        let mut session = Self {
            tracks: library.tracks,
            liked_order,
            playlists: library.playlists,
            view: library.view,
            current: library.current,
            playing: library.was_playing,
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
            order_mode: OrderMode::Shuffle,
            playlist_density: PlaylistDensity::Normal,
            playback_error: None,
            playback_error_until: None,
            cue_containers: HashSet::new(),
            list_epoch: 0,
            index_by_id: HashMap::new(),
            playlist_of: HashMap::new(),
            search_folds: Vec::new(),
            engine: None,
            rack: AudioRack::flat(),
            armed_at: None,
            heard_to: library.position,
            hold: false,
            playing_playlist: library.playing_playlist_id,
            playback_saved_at: None,
            playback_stamp: None,
            db: Some(library.db),
        };
        session.rebuild_indexes();
        session.restore_queue();
        session
    }

    fn empty() -> Self {
        Self {
            tracks: Vec::new(),
            liked_order: Vec::new(),
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
            order_mode: OrderMode::Shuffle,
            playlist_density: PlaylistDensity::Normal,
            playback_error: None,
            playback_error_until: None,
            cue_containers: HashSet::new(),
            list_epoch: 0,
            index_by_id: HashMap::new(),
            playlist_of: HashMap::new(),
            search_folds: Vec::new(),
            engine: None,
            rack: AudioRack::flat(),
            armed_at: None,
            heard_to: 0.0,
            hold: false,
            playing_playlist: None,
            playback_saved_at: None,
            playback_stamp: None,
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
            liked_order: Vec::new(),
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
            order_mode: OrderMode::Shuffle,
            playlist_density: PlaylistDensity::Normal,
            playback_error: None,
            playback_error_until: None,
            cue_containers: HashSet::new(),
            list_epoch: 0,
            index_by_id: HashMap::new(),
            playlist_of: HashMap::new(),
            search_folds: Vec::new(),
            engine: None,
            rack: AudioRack::flat(),
            armed_at: None,
            heard_to: 0.0,
            hold: false,
            playing_playlist: None,
            playback_saved_at: None,
            playback_stamp: None,
            db: None,
        };
        session.rebuild_indexes();
        session
    }

    fn rebuild_indexes(&mut self) {
        self.sync_liked_order();
        catalog::inherit_cue_covers(&mut self.tracks);
        self.cue_containers = catalog::cue_container_ids(&self.tracks);
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
        self.list_epoch = self.list_epoch.wrapping_add(1);
    }

    /// Drop likes that no longer exist and append newly liked rows at the end.
    fn sync_liked_order(&mut self) {
        let liked: HashSet<TrackId> = self
            .tracks
            .iter()
            .filter(|track| track.liked)
            .map(|track| track.id)
            .collect();
        self.liked_order.retain(|id| liked.contains(id));
        let known: HashSet<TrackId> = self.liked_order.iter().copied().collect();
        for track in &self.tracks {
            if track.liked && !known.contains(&track.id) {
                self.liked_order.push(track.id);
            }
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

    /// Changes when the rows of a view can change. Position updates do not.
    pub fn list_epoch(&self) -> u64 {
        self.list_epoch
    }

    pub fn current_track(&self) -> Option<&Track> {
        self.current.and_then(|id| self.track(id))
    }

    pub fn window_title(&self) -> String {
        match self.current_track() {
            Some(track) => format_window_title(&track.artist, &track.title),
            None => "muzeeka".into(),
        }
    }

    /// Play is available, and whether previous and next have somewhere to go.
    pub fn taskbar_flags(&self) -> (bool, bool, bool, bool) {
        let (has_prev, has_next) = self.can_navigate();
        let can_play = self.current.is_some() || !self.queue.is_empty();
        (self.playing, can_play, has_prev, has_next)
    }

    fn can_navigate(&self) -> (bool, bool) {
        let Some(current) = self.current else {
            return (false, false);
        };
        if self.queue.is_empty() {
            return (self.position > 2.0, false);
        }
        if self.random_jumps() {
            let any = !self.queue.is_empty();
            return (any, self.repeat == RepeatMode::One || any);
        }
        let deck = self.shuffled_deck();
        let repeat_all = self.repeat == RepeatMode::All;
        let order = self.play_order();
        // Repeat-one restarts the open track, so next still has somewhere to go.
        let has_next =
            self.repeat == RepeatMode::One || step_forward(order, current, repeat_all).is_some();
        let has_prev =
            self.position > 2.0 || step_back(order, current, !deck && repeat_all).is_some();
        (has_prev, has_next)
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
            LibraryView::Liked => self.liked_indexes(&parsed),
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
                let (Some(left), Some(right)) = (self.tracks.get(*left), self.tracks.get(*right))
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

    fn liked_indexes(&self, query: &ParsedQuery) -> Vec<usize> {
        self.liked_order
            .iter()
            .filter_map(|id| {
                let index = self.index_by_id.get(id).copied()?;
                let track = self.tracks.get(index)?;
                (track.liked
                    && self.matches_playlist_filter(track, query)
                    && matches_query(track, query))
                .then_some(index)
            })
            .collect()
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
            .filter(|fold| !self.cue_containers.contains(&fold.id))
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
            LibraryView::All => !self.cue_containers.contains(&track.id),
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
        let Some(liked) = self
            .tracks
            .iter()
            .find(|track| track.id == id)
            .map(|track| track.liked)
        else {
            return;
        };
        self.set_liked(id, !liked);
    }

    fn set_liked(&mut self, id: TrackId, liked: bool) {
        let Some(current) = self
            .tracks
            .iter()
            .find(|track| track.id == id)
            .map(|track| track.liked)
        else {
            return;
        };
        if current == liked {
            return;
        }
        if let Err(error) = self.write_like(id, liked) {
            self.push_notice(error);
            return;
        }
        if let Some(track) = self.tracks.iter_mut().find(|track| track.id == id) {
            track.liked = liked;
        }
        if liked {
            if !self.liked_order.contains(&id) {
                self.liked_order.push(id);
            }
        } else {
            self.liked_order.retain(|item| *item != id);
        }
        self.list_epoch = self.list_epoch.wrapping_add(1);
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
            db.execute(
                "DELETE FROM liked_tracks WHERE track_id = ?1",
                [id.0 as i64],
            )
            .map_err(|error| format!("Could not remove like: {error}"))?;
        }
        Ok(())
    }

    pub fn create_playlist(&mut self) {
        let n = self.playlists.len() + 1;
        let id = format!("playlist-{n}");
        let name = format!("Playlist {n}");
        if let Err(error) = self.insert_playlist(&id, &name) {
            self.push_notice(error);
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
        let Some(playlist) = self
            .playlists
            .iter()
            .find(|playlist| playlist.id == playlist_id)
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
        let Some(playlist) = self
            .playlists
            .iter()
            .find(|playlist| playlist.id == playlist_id)
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

    /// Reorder the open list. The pointer index is in the order on screen,
    /// including a column sort. The sort is cleared, because that screen order
    /// becomes the stored order.
    pub(crate) fn reorder_view(&mut self, moving: &[TrackId], insert_at: usize) -> bool {
        let displayed = self.displayed_ids();
        let next = crate::ui::reorder::reorder_at_boundary(&displayed, moving, insert_at);
        if next == displayed {
            return false;
        }
        let view = self.view.clone();
        let playing = self.playing_source(&view);
        let saved = match &view {
            LibraryView::Playlist(id) => self.write_playlist_track_positions(id, &next),
            LibraryView::Liked => self.write_liked_positions(&next),
            LibraryView::All => {
                let order = self.spliced_library_order(&next);
                self.write_library_positions(&order)
            }
        };
        if let Err(error) = saved {
            self.push_notice(error);
            return false;
        }
        match &view {
            LibraryView::Playlist(id) => {
                if let Some(playlist) = self
                    .playlists
                    .iter_mut()
                    .find(|playlist| &playlist.id == id)
                {
                    playlist.track_ids = next.clone();
                }
            }
            LibraryView::Liked => self.liked_order = next.clone(),
            LibraryView::All => {
                let order = self.spliced_library_order(&next);
                self.apply_track_order(&order);
            }
        }
        self.sort = None;
        self.rebuild_indexes();
        if playing {
            self.sync_playing_queue(&view);
        }
        true
    }

    /// Add tracks to a playlist. Tracks already there are left where they are.
    pub(crate) fn copy_tracks(&mut self, ids: &[TrackId], target: &str) -> usize {
        if !self.playlists.iter().any(|playlist| playlist.id == target) {
            return 0;
        }
        self.unique_ids(ids)
            .into_iter()
            .filter(|id| self.add_to_playlist(target, *id))
            .count()
    }

    /// Move tracks onto a playlist. Ctrl is the copy path. A move out of a
    /// playlist takes the rows with it. A move out of Liked unlikes them.
    /// A move out of All takes them out of every other playlist.
    pub(crate) fn move_tracks(
        &mut self,
        ids: &[TrackId],
        target: &str,
        source: &LibraryView,
    ) -> usize {
        if !self.playlists.iter().any(|playlist| playlist.id == target) {
            return 0;
        }
        if let LibraryView::Playlist(id) = source {
            if id == target {
                return 0;
            }
        }
        let ids = self.unique_ids(ids);
        if ids.is_empty() {
            return 0;
        }
        let count = ids.len();
        let playing_playlist = self.playing_playlist.clone();
        let playing_source = self.playing_source(source);
        for id in &ids {
            let _ = self.add_to_playlist(target, *id);
        }
        match source {
            LibraryView::Playlist(id) => {
                let stop = self.current.is_some_and(|current| ids.contains(&current));
                for track in &ids {
                    self.remove_from_playlist(id, *track);
                }
                if stop {
                    self.stop_playback();
                } else if playing_playlist.as_deref() == Some(id.as_str()) {
                    self.drop_ids_from_queue(&ids);
                }
            }
            LibraryView::Liked => {
                for track in &ids {
                    self.set_liked(*track, false);
                }
                if playing_source {
                    self.sync_playing_queue(&LibraryView::Liked);
                }
            }
            LibraryView::All => {
                let others: Vec<String> = self
                    .playlists
                    .iter()
                    .filter(|playlist| playlist.id != target)
                    .map(|playlist| playlist.id.clone())
                    .collect();
                for playlist in &others {
                    for track in &ids {
                        self.remove_from_playlist(playlist, *track);
                    }
                }
                if let Some(playing) = playing_playlist {
                    if playing != target {
                        self.sync_playing_queue(&LibraryView::Playlist(playing));
                    }
                }
            }
        }
        count
    }

    /// Sidebar order. `insert_at` is a boundary in the current playlist list.
    pub(crate) fn reorder_playlists(&mut self, moving: &[String], insert_at: usize) -> bool {
        let ids: Vec<String> = self
            .playlists
            .iter()
            .map(|playlist| playlist.id.clone())
            .collect();
        let next = crate::ui::reorder::reorder_at_boundary(&ids, moving, insert_at);
        if next == ids {
            return false;
        }
        if let Err(error) = self.write_playlist_positions(&next) {
            self.push_notice(error);
            return false;
        }
        let mut by_id: HashMap<String, Playlist> = self
            .playlists
            .drain(..)
            .map(|playlist| (playlist.id.clone(), playlist))
            .collect();
        self.playlists = next.iter().filter_map(|id| by_id.remove(id)).collect();
        true
    }

    fn displayed_ids(&self) -> Vec<TrackId> {
        self.visible_indexes("")
            .into_iter()
            .filter_map(|index| self.tracks.get(index).map(|track| track.id))
            .collect()
    }

    fn unique_ids(&self, ids: &[TrackId]) -> Vec<TrackId> {
        let mut seen = HashSet::new();
        ids.iter()
            .copied()
            .filter(|id| self.track(*id).is_some() && seen.insert(*id))
            .collect()
    }

    /// Library order with the visible rows replaced by `visible`, hidden rows
    /// staying in their slots. Cue parents stay put while All tracks move.
    fn spliced_library_order(&self, visible: &[TrackId]) -> Vec<TrackId> {
        let visible_set: HashSet<TrackId> = visible.iter().copied().collect();
        let mut upcoming = visible.iter().copied();
        self.tracks
            .iter()
            .map(|track| {
                if visible_set.contains(&track.id) {
                    upcoming.next().unwrap_or(track.id)
                } else {
                    track.id
                }
            })
            .collect()
    }

    fn apply_track_order(&mut self, order: &[TrackId]) {
        let mut by_id: HashMap<TrackId, Track> = self
            .tracks
            .drain(..)
            .map(|track| (track.id, track))
            .collect();
        self.tracks = order.iter().filter_map(|id| by_id.remove(id)).collect();
        for track in by_id.into_values() {
            self.tracks.push(track);
        }
    }

    /// Playlist whose queue is playing, when playback started from one.
    pub(crate) fn bars_playlist(&self) -> Option<&str> {
        self.playing
            .then(|| self.playing_playlist.as_deref())
            .flatten()
    }

    /// Where a transport-bar drag should move or copy from.
    pub(crate) fn drag_source(&self) -> LibraryView {
        if let Some(id) = &self.playing_playlist {
            return LibraryView::Playlist(id.clone());
        }
        if self.queue_matches(&LibraryView::Liked) {
            LibraryView::Liked
        } else {
            LibraryView::All
        }
    }

    fn playing_source(&self, source: &LibraryView) -> bool {
        match source {
            LibraryView::Playlist(id) => self.playing_playlist.as_deref() == Some(id.as_str()),
            LibraryView::All | LibraryView::Liked => {
                self.playing_playlist.is_none() && self.queue_matches(source)
            }
        }
    }

    fn queue_matches(&self, view: &LibraryView) -> bool {
        let natural = self.natural_ids(view);
        let queued: HashSet<TrackId> = self.queue.iter().copied().collect();
        let base: HashSet<TrackId> = natural.iter().copied().collect();
        if queued == base {
            return true;
        }
        if let Some(current) = self.current {
            let mut with_current = base;
            with_current.insert(current);
            return queued == with_current;
        }
        false
    }

    fn natural_ids(&self, view: &LibraryView) -> Vec<TrackId> {
        match view {
            LibraryView::Playlist(id) => self
                .playlists
                .iter()
                .find(|playlist| &playlist.id == id)
                .map(|playlist| playlist.track_ids.clone())
                .unwrap_or_default(),
            LibraryView::Liked => self.liked_order.clone(),
            LibraryView::All => self
                .tracks
                .iter()
                .filter(|track| !self.cue_containers.contains(&track.id))
                .map(|track| track.id)
                .collect(),
        }
    }

    fn sync_playing_queue(&mut self, view: &LibraryView) {
        let mut next = self.natural_ids(view);
        if let Some(current) = self.current {
            if !next.contains(&current) {
                next.insert(0, current);
            }
        }
        self.adopt_queue(next);
    }

    fn drop_ids_from_queue(&mut self, ids: &[TrackId]) {
        let gone: HashSet<TrackId> = ids.iter().copied().collect();
        self.queue.retain(|id| !gone.contains(id));
        self.shuffle_order.retain(|id| !gone.contains(id));
        self.refresh_gapless();
    }

    fn adopt_queue(&mut self, next: Vec<TrackId>) {
        if self.queue == next {
            return;
        }
        let previous: HashSet<TrackId> = self.queue.iter().copied().collect();
        let same_set = next.len() == previous.len() && next.iter().all(|id| previous.contains(id));
        self.queue = next;
        if self.shuffle && !same_set {
            self.rebuild_shuffle();
        }
        self.refresh_gapless();
    }

    fn stop_playback(&mut self) {
        self.playing = false;
        self.current = None;
        self.position = 0.0;
        self.queue.clear();
        self.shuffle_order.clear();
        self.playing_playlist = None;
        self.output_pause();
        self.remember_playback(true);
    }

    fn write_playlist_track_positions(&mut self, id: &str, ids: &[TrackId]) -> Result<(), String> {
        let Some(db) = &mut self.db else {
            return Ok(());
        };
        let tx = db
            .transaction()
            .map_err(|error| format!("Could not reorder: {error}"))?;
        for (position, track) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE playlist_tracks SET position = ?3
                  WHERE playlist_id = ?1 AND track_id = ?2",
                rusqlite::params![id, track.0 as i64, position as i64],
            )
            .map_err(|error| format!("Could not reorder: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("Could not reorder: {error}"))?;
        Ok(())
    }

    fn write_library_positions(&mut self, ids: &[TrackId]) -> Result<(), String> {
        let Some(db) = &mut self.db else {
            return Ok(());
        };
        let tx = db
            .transaction()
            .map_err(|error| format!("Could not reorder: {error}"))?;
        for (position, track) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE tracks SET library_position = ?2 WHERE id = ?1",
                rusqlite::params![track.0 as i64, position as i64],
            )
            .map_err(|error| format!("Could not reorder: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("Could not reorder: {error}"))?;
        Ok(())
    }

    fn write_liked_positions(&mut self, ids: &[TrackId]) -> Result<(), String> {
        let Some(db) = &mut self.db else {
            return Ok(());
        };
        let tx = db
            .transaction()
            .map_err(|error| format!("Could not reorder: {error}"))?;
        for (position, track) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE liked_tracks SET position = ?2 WHERE track_id = ?1",
                rusqlite::params![track.0 as i64, position as i64],
            )
            .map_err(|error| format!("Could not reorder: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("Could not reorder: {error}"))?;
        Ok(())
    }

    fn write_playlist_positions(&mut self, ids: &[String]) -> Result<(), String> {
        let Some(db) = &mut self.db else {
            return Ok(());
        };
        let tx = db
            .transaction()
            .map_err(|error| format!("Could not reorder playlists: {error}"))?;
        for (position, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE playlists SET position = ?2 WHERE id = ?1",
                rusqlite::params![id, position as i64],
            )
            .map_err(|error| format!("Could not reorder playlists: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("Could not reorder playlists: {error}"))?;
        Ok(())
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

    /// Writes scanned files into the open library and refreshes the in-memory lists.
    /// Playback keeps the current file when that row is still present.
    pub fn import_dropped(
        &mut self,
        files: &[MusicFile],
        kind: &ImportKind,
    ) -> Result<import::ImportReport, String> {
        let (report, tracks, playlists) = {
            let db = self
                .db
                .as_mut()
                .ok_or_else(|| "Library database is not open".to_string())?;
            let report = import::commit(db, files, kind)?;
            let (tracks, playlists) = catalog::read_lists(db)?;
            (report, tracks, playlists)
        };
        if let Some(id) = &report.remember_download_playlist {
            crate::ui::ytdlp::remember_download_playlist(id);
        }
        self.install_lists(tracks, playlists, report.open_id.clone());
        Ok(report)
    }

    pub(crate) fn has_library(&self) -> bool {
        self.db.is_some()
    }

    /// Drops in-memory rows staged before a scan when the database write fails.
    pub(crate) fn reload_lists(&mut self) -> Result<(), String> {
        let open = match &self.view {
            LibraryView::Playlist(id) => Some(id.clone()),
            LibraryView::All | LibraryView::Liked => None,
        };
        let db = self
            .db
            .as_mut()
            .ok_or_else(|| "Library database is not open".to_string())?;
        let (tracks, playlists) = catalog::read_lists(db)?;
        self.install_lists(tracks, playlists, open);
        Ok(())
    }

    /// Shows dropped audio in the open list before tags are read.
    /// A file dropped on All tracks stays in that list. A file dropped on a
    /// playlist opens that playlist and appends the row.
    pub(crate) fn stage_dropped_files(
        &mut self,
        paths: &[String],
        playlist_id: Option<&str>,
        mut at: Option<usize>,
    ) {
        if let Some(id) = playlist_id {
            if self.playlists.iter().any(|playlist| playlist.id == id) {
                self.view = LibraryView::Playlist(id.to_string());
            }
        } else if matches!(self.view, LibraryView::Liked) {
            self.view = LibraryView::All;
        }
        for raw in paths {
            let path = raw.trim();
            if path.is_empty() || !import::is_audio_drop(path) {
                continue;
            }
            let key = crate::ui::paths::path_key(path);
            if let Some(existing) = self
                .tracks
                .iter()
                .find(|track| crate::ui::paths::path_key(&track.path) == key)
                .map(|track| track.id)
            {
                if self.link_staged(playlist_id, existing, at) {
                    at = at.map(|index| index + 1);
                }
                continue;
            }
            let id = next_placeholder_id(&self.tracks);
            let title = std::path::Path::new(path)
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("Track")
                .to_string();
            self.tracks.push(Track {
                id,
                title,
                artist: String::new(),
                album: String::new(),
                duration: 0.0,
                liked: false,
                path: path.to_string(),
                cover: None,
                lyrics: None,
            });
            if self.link_staged(playlist_id, id, at) {
                at = at.map(|index| index + 1);
            }
        }
        self.rebuild_indexes();
    }

    /// Inserts `id` at `at` when that index is set. Returns whether the row was new.
    fn link_staged(&mut self, playlist_id: Option<&str>, id: TrackId, at: Option<usize>) -> bool {
        let Some(playlist_id) = playlist_id else {
            let Some(at) = at else {
                return false;
            };
            let Some(pos) = self.tracks.iter().position(|track| track.id == id) else {
                return false;
            };
            let track = self.tracks.remove(pos);
            let at = at.min(self.tracks.len());
            self.tracks.insert(at, track);
            return true;
        };
        let Some(playlist) = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == playlist_id)
        else {
            return false;
        };
        if playlist.track_ids.contains(&id) {
            return false;
        }
        let at = at
            .unwrap_or(playlist.track_ids.len())
            .min(playlist.track_ids.len());
        playlist.track_ids.insert(at, id);
        true
    }

    fn install_lists(
        &mut self,
        tracks: Vec<Track>,
        playlists: Vec<Playlist>,
        open_id: Option<String>,
    ) {
        let live: HashSet<TrackId> = tracks.iter().map(|track| track.id).collect();
        self.tracks = tracks;
        self.playlists = playlists;
        self.queue.retain(|id| live.contains(id));
        self.shuffle_order.retain(|id| live.contains(id));
        if self.current.is_some_and(|id| !live.contains(&id)) {
            self.current = None;
            self.playing = false;
            self.position = 0.0;
            self.output_pause();
        }
        if self
            .playing_playlist
            .as_ref()
            .is_some_and(|id| !self.playlists.iter().any(|playlist| &playlist.id == id))
        {
            self.playing_playlist = None;
        }
        if let Some(id) = open_id {
            if self.playlists.iter().any(|playlist| playlist.id == id) {
                self.view = LibraryView::Playlist(id);
            }
        }
        if let Some(db) = &self.db {
            if let Ok(order) = catalog::load_liked_order(db) {
                self.liked_order = order;
            }
        } else {
            self.liked_order.clear();
        }
        self.search_folds.clear();
        self.rebuild_indexes();
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
        let queue = if queue.iter().any(|queued| *queued == id) {
            queue
        } else {
            let mut queue = queue;
            queue.insert(0, id);
            queue
        };
        let same_queue = self.queue == queue;
        self.queue = queue;
        self.current = Some(id);
        self.position = 0.0;
        self.playing = true;
        self.playing_playlist = self.active_playlist_id();
        // A new playlist gets a new shuffle. The same queue keeps its order,
        // so previous and next still walk the deck after another track is opened.
        if self.shuffled_deck() && (!same_queue || !self.order_matches()) {
            self.rebuild_shuffle();
        }
        if self.random_jumps() {
            self.shuffle_order.clear();
        }
        self.output_play(0.0);
        self.remember_playback(true);
    }

    /// Start `id` from its first track. Shuffle starts on a random track instead.
    /// The open library page stays put; next and previous still walk this playlist.
    pub fn play_playlist(&mut self, id: &str) {
        let Some(ids) = self
            .playlists
            .iter()
            .find(|playlist| playlist.id == id)
            .map(|playlist| playlist.track_ids.clone())
        else {
            return;
        };
        if ids.is_empty() {
            return;
        }
        let start = if self.shuffle {
            ids[(self.rand() as usize) % ids.len()]
        } else {
            ids[0]
        };
        self.play(start, ids);
        if self.playing_playlist.as_deref() != Some(id) {
            self.playing_playlist = Some(id.to_string());
            self.remember_playback(true);
        }
    }

    fn active_playlist_id(&self) -> Option<String> {
        match &self.view {
            LibraryView::Playlist(id) => Some(id.clone()),
            LibraryView::All | LibraryView::Liked => None,
        }
    }

    /// Put the saved track back into a queue so next and previous still have a list.
    fn restore_queue(&mut self) {
        let Some(current) = self.current else {
            return;
        };
        let from_playlist = self.playing_playlist.as_deref().and_then(|id| {
            self.playlists
                .iter()
                .find(|playlist| playlist.id == id)
                .map(|playlist| playlist.track_ids.clone())
        });
        let mut ids = if let Some(ids) = from_playlist {
            ids
        } else {
            match &self.view {
                LibraryView::Liked => self.liked_order.clone(),
                LibraryView::Playlist(id) => self
                    .playlists
                    .iter()
                    .find(|playlist| &playlist.id == id)
                    .map(|playlist| playlist.track_ids.clone())
                    .unwrap_or_default(),
                LibraryView::All => self.tracks.iter().map(|track| track.id).collect(),
            }
        };
        if !ids.iter().any(|id| *id == current) {
            ids.insert(0, current);
        }
        self.queue = ids;
        if self.shuffle {
            self.rebuild_shuffle();
        }
    }

    /// Write the open track and playhead into `app_state`.
    /// A forced write runs on track changes, seek, and exit. The playhead
    /// itself is stored at most every few seconds.
    fn remember_playback(&mut self, force: bool) {
        if self.db.is_none() {
            return;
        }
        let file = self.current_track().map(|track| track.path.clone());
        let position = if file.is_some() {
            self.position.max(0.0)
        } else {
            0.0
        };
        let playing = self.playing && file.is_some();
        let stamp = PlaybackStamp {
            file: file.clone(),
            second: position.floor() as i64,
            playing,
        };
        if !force {
            if self.playback_stamp.as_ref() == Some(&stamp) {
                return;
            }
            if self
                .playback_saved_at
                .is_some_and(|saved| saved.elapsed() < Duration::from_secs(5))
            {
                return;
            }
        }
        let memory = catalog::PlaybackMemory {
            active_playlist_id: self.active_playlist_id(),
            playing_playlist_id: self.playing_playlist.clone(),
            current_file: file,
            volume: self.volume,
            shuffle: self.shuffle,
            repeat: self.repeat,
            position,
            was_playing: playing,
        };
        let saved = catalog::save_playback(self.db.as_ref().expect("db"), &memory);
        match saved {
            Ok(()) => {
                self.playback_stamp = Some(stamp);
                self.playback_saved_at = Some(Instant::now());
            }
            Err(error) => eprintln!("Could not save playback: {error}"),
        }
    }

    /// The transport button only turns the saved order on or off.
    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        self.sync_shuffle_order();
        self.refresh_gapless();
        self.remember_playback(true);
    }

    /// Settings pick Shuffle or Random. The transport switch stays as it is.
    pub fn set_order_mode(&mut self, mode: OrderMode) {
        if self.order_mode == mode {
            return;
        }
        self.order_mode = mode;
        if self.shuffle {
            self.sync_shuffle_order();
            self.refresh_gapless();
        }
    }

    fn shuffled_deck(&self) -> bool {
        self.shuffle && self.order_mode == OrderMode::Shuffle
    }

    fn random_jumps(&self) -> bool {
        self.shuffle && self.order_mode == OrderMode::Random
    }

    fn sync_shuffle_order(&mut self) {
        if self.shuffled_deck() {
            self.rebuild_shuffle();
        } else {
            self.shuffle_order.clear();
        }
    }

    fn play_order(&self) -> &[TrackId] {
        if self.shuffled_deck() {
            &self.shuffle_order
        } else {
            &self.queue
        }
    }

    fn order_matches(&self) -> bool {
        if self.shuffle_order.len() != self.queue.len() {
            return false;
        }
        let mut left: Vec<u32> = self.shuffle_order.iter().map(|id| id.0).collect();
        let mut right: Vec<u32> = self.queue.iter().map(|id| id.0).collect();
        left.sort_unstable();
        right.sort_unstable();
        left == right
    }

    /// Repeat applies to the song that is already open.
    pub fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.next();
        self.refresh_gapless();
        self.remember_playback(true);
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
            self.remember_playback(true);
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
        // Random never restarts the open track. Forward and back are both a new pick.
        if self.position > 2.0 && !self.random_jumps() {
            self.position = 0.0;
            self.playing = true;
            if self.output_holds_current() {
                self.output_seek(0.0);
                self.output_resume();
            } else {
                self.output_play(0.0);
            }
            self.remember_playback(true);
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
            self.remember_playback(true);
            return;
        }
        if self.queue.is_empty() {
            self.playing = false;
            self.output_pause();
            self.remember_playback(true);
            return;
        }
        if self.random_jumps() {
            let Some(id) = self.pick_random(current) else {
                self.playing = false;
                self.output_pause();
                self.remember_playback(true);
                return;
            };
            self.current = Some(id);
            self.position = 0.0;
            self.playing = true;
            self.output_play(0.0);
            self.remember_playback(true);
            return;
        }
        let deck = self.shuffled_deck();
        if deck && !self.order_matches() {
            self.rebuild_shuffle();
        }
        let repeat_all = self.repeat == RepeatMode::All;
        let wrap = !deck && repeat_all;
        let next = if direction > 0 {
            step_forward(self.play_order(), current, repeat_all)
        } else {
            step_back(self.play_order(), current, wrap)
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
        self.remember_playback(true);
    }

    pub fn seek(&mut self, fraction: f32) {
        let Some(track) = self.current_track() else {
            return;
        };
        self.position = (fraction as f64).clamp(0.0, 1.0) * track.duration;
        self.heard_to = self.position;
        self.output_seek(self.position);
        self.remember_playback(true);
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
        self.remember_playback(true);
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if self.volume > 0.0 {
            self.last_volume = self.volume;
        }
        self.output_volume();
        self.remember_playback(false);
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
        self.remember_playback(true);
    }

    /// Advances playback. With BASS attached, the position comes from the
    /// engine. Without it, this is the preview clock used by tests.
    pub fn tick(&mut self, dt: f64) -> bool {
        let expired = self.expire_playback_error();
        if self.engine.is_some() {
            let changed = self.poll_output();
            if self.playing {
                self.remember_playback(false);
            }
            return changed || expired;
        }
        if !self.playing {
            return expired;
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
                        self.remember_playback(true);
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
                    self.remember_playback(true);
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
        let played = self.engine.as_ref().expect("engine").play(&path, queue);
        if let Err(error) = played {
            self.playing = false;
            self.hold = true;
            self.note_playback_error(error);
            return;
        }
        self.clear_playback_error();
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
    /// Repeat-one stays on this file. A shuffled deck follows `shuffle_order`.
    /// Random does not preload, because the next pick does not exist yet.
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
        if self.repeat == RepeatMode::One || self.queue.is_empty() || self.random_jumps() {
            return vec![id];
        }
        gapless_window(self.play_order(), id, self.repeat == RepeatMode::All)
    }

    /// Any queue track except the one that is open, when there is another.
    fn pick_random(&mut self, current: TrackId) -> Option<TrackId> {
        let len = self.queue.len();
        if len == 0 {
            return None;
        }
        if len == 1 {
            return Some(self.queue[0]);
        }
        let start = (self.rand() as usize) % len;
        for step in 0..len {
            let id = self.queue[(start + step) % len];
            if id != current {
                return Some(id);
            }
        }
        None
    }

    fn track_id_for_path(&self, path: &str) -> Option<TrackId> {
        self.queue
            .iter()
            .copied()
            .find(|id| {
                self.track(*id)
                    .is_some_and(|track| playback::same_audio_path(&track.path, path))
            })
            .or_else(|| {
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

const PLAYBACK_ERROR_TTL: Duration = Duration::from_secs(8);

/// Taskbar and window caption: `artist - title | muzeeka`.
pub fn format_window_title(artist: &str, title: &str) -> String {
    let artist = artist.trim();
    let title = title.trim();
    match (artist.is_empty(), title.is_empty()) {
        (true, true) => "muzeeka".into(),
        (true, false) => format!("{title} | muzeeka"),
        (false, true) => format!("{artist} | muzeeka"),
        (false, false) => format!("{artist} - {title} | muzeeka"),
    }
}

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

/// True when a stopped stream is the end of the file, including a rewind to zero.
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

/// Time left on the open track, drawn with a leading minus (`-1:35`).
pub fn format_remaining(position: f64, duration: f64) -> String {
    let left = if position.is_finite() && duration.is_finite() {
        (duration - position).max(0.0)
    } else {
        0.0
    };
    format!("-{}", format_time(left))
}

/// `settings.json` key `remaining_time`: `"1"` counts down, anything else elapsed.
pub(crate) fn load_remaining_time() -> bool {
    let path = catalog::app_data_dir().join("settings.json");
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    value.get("remaining_time").and_then(|item| item.as_str()) == Some("1")
}

pub(crate) fn store_remaining_time(on: bool) {
    let path = catalog::app_data_dir().join("settings.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
    let flag = if on { "1" } else { "0" };
    let Ok(next) = crate::ui::ytdlp::merge_setting(&raw, "remaining_time", Some(flag)) else {
        return;
    };
    if next != raw {
        let _ = fs::write(path, next);
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

impl Drop for Session {
    fn drop(&mut self) {
        // The playhead is written at most every few seconds. Closing the
        // window is the last chance to store the position the throttle skipped.
        self.remember_playback(true);
    }
}

fn next_placeholder_id(tracks: &[Track]) -> TrackId {
    static NEXT: AtomicU32 = AtomicU32::new(0xF000_0000);
    for _ in 0..64 {
        let id = TrackId(NEXT.fetch_add(1, Ordering::Relaxed));
        if tracks.iter().all(|track| track.id != id) {
            return id;
        }
    }
    TrackId(NEXT.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::catalog::{self, LoadedLibrary};
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn remaining_time_counts_down_with_a_minus() {
        assert_eq!(format_remaining(10.0, 105.2), "-1:35");
        assert_eq!(format_remaining(0.0, 3_723.0), "-1:02:03");
        assert_eq!(format_remaining(90.0, 30.0), "-0:00");
    }

    #[test]
    fn dropped_file_shows_in_the_list_before_the_scan() {
        let mut session = Session::preview();
        let before = session.tracks.len();
        session.stage_dropped_files(&[r"D:\Music\new song.mp3".into()], None, None);
        assert_eq!(session.tracks.len(), before + 1);
        assert_eq!(session.view, LibraryView::All);
        assert_eq!(session.tracks.last().unwrap().title, "new song");
        assert!(session.visible_indexes("").iter().any(|index| {
            session
                .track_by_index(*index)
                .is_some_and(|track| track.title == "new song")
        }));

        session.stage_dropped_files(&[r"D:\Music\new song.mp3".into()], Some("focus"), Some(0));
        assert_eq!(
            session
                .playlists
                .iter()
                .find(|playlist| playlist.id == "focus")
                .unwrap()
                .track_ids
                .first()
                .copied(),
            session
                .tracks
                .iter()
                .rev()
                .find(|track| track.title == "new song")
                .map(|track| track.id)
        );
        assert_eq!(session.tracks.len(), before + 1);
        assert_eq!(session.view, LibraryView::Playlist("focus".into()));
        let id = session.tracks.last().unwrap().id;
        let focus = session
            .playlists
            .iter()
            .find(|playlist| playlist.id == "focus")
            .unwrap();
        assert!(focus.track_ids.contains(&id));
    }

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

    fn ids_of(session: &Session) -> Vec<u32> {
        session
            .visible_indexes("")
            .into_iter()
            .filter_map(|index| session.track_by_index(index).map(|track| track.id.0))
            .collect()
    }

    #[test]
    fn reordering_a_playlist_moves_the_row_and_clears_sort() {
        let mut session = Session::preview();
        session.view = LibraryView::Playlist("focus".into());
        session.sort = Some(Sort {
            column: SortColumn::Title,
            descending: false,
        });
        let before = ids_of(&session);
        let moved = TrackId(before[0]);
        assert!(session.reorder_view(&[moved], before.len()));
        assert!(session.sort.is_none());
        let after = ids_of(&session);
        assert_eq!(after.last().copied(), Some(moved.0));
        assert_eq!(after.len(), before.len());
        let focus = session
            .playlists
            .iter()
            .find(|playlist| playlist.id == "focus")
            .unwrap();
        assert_eq!(focus.track_ids.last().copied(), Some(moved));
    }

    #[test]
    fn reordering_all_tracks_leaves_a_cue_parent_in_its_slot() {
        let mut session = Session::preview();
        session.tracks[0].path = r"D:\Album\disc.flac".into();
        session.tracks[1].path = r"D:\Album\disc.flac#cue:1".into();
        session.rebuild_indexes();
        session.view = LibraryView::All;
        let parent = session.tracks[0].id;
        assert!(session.cue_containers.contains(&parent));
        let visible = ids_of(&session);
        let moved = TrackId(*visible.last().unwrap());
        assert!(session.reorder_view(&[moved], 0));
        assert_eq!(session.tracks[0].id, parent);
        assert_eq!(ids_of(&session).first().copied(), Some(moved.0));
    }

    #[test]
    fn reordering_liked_tracks_does_not_move_the_library() {
        let mut session = Session::preview();
        let library: Vec<u32> = session.tracks.iter().map(|track| track.id.0).collect();
        session.view = LibraryView::Liked;
        let liked = ids_of(&session);
        let moved = TrackId(liked[0]);
        assert!(session.reorder_view(&[moved], liked.len()));
        assert_eq!(
            session
                .tracks
                .iter()
                .map(|track| track.id.0)
                .collect::<Vec<_>>(),
            library
        );
        assert_eq!(ids_of(&session).last().copied(), Some(moved.0));
    }

    #[test]
    fn play_playlist_starts_at_the_first_track_until_shuffle_is_on() {
        let mut session = Session::preview();
        session.view = LibraryView::All;
        session.play_playlist("missing");
        assert!(session.current.is_none());
        session.play_playlist("focus");
        let focus: Vec<_> = (4..12).map(TrackId).collect();
        assert_eq!(session.queue, focus);
        assert_eq!(session.current, Some(TrackId(4)));
        assert!(session.playing);
        assert_eq!(session.playing_playlist.as_deref(), Some("focus"));
        assert!(matches!(session.view, LibraryView::All));

        session.shuffle = true;
        session.rng = 1;
        session.play_playlist("focus");
        assert_eq!(session.queue, focus);
        assert_eq!(session.current, Some(TrackId(5)));
        assert_eq!(session.playing_playlist.as_deref(), Some("focus"));
    }

    #[test]
    fn reordering_the_playing_playlist_updates_the_queue_and_keeps_the_deck() {
        let mut session = Session::preview();
        session.view = LibraryView::Playlist("night".into());
        let queue = ids_of(&session)
            .into_iter()
            .map(TrackId)
            .collect::<Vec<_>>();
        session.play(queue[0], queue.clone());
        session.shuffle = true;
        session.rebuild_shuffle();
        let deck = session.shuffle_order.clone();
        let moved = queue[0];
        assert!(session.reorder_view(&[moved], queue.len()));
        assert_eq!(session.queue.first().copied(), Some(queue[1]));
        assert_eq!(session.queue.last().copied(), Some(moved));
        assert_eq!(session.shuffle_order, deck);
    }

    #[test]
    fn copying_tracks_leaves_the_source_playlist() {
        let mut session = Session::preview();
        let id = TrackId(0);
        assert_eq!(session.copy_tracks(&[id], "focus"), 1);
        assert_eq!(session.copy_tracks(&[id], "focus"), 0);
        assert!(session.playlists[0].track_ids.contains(&id));
        assert!(session.playlists[1].track_ids.contains(&id));
    }

    #[test]
    fn moving_tracks_out_of_a_playlist_removes_them_and_stops_the_open_one() {
        let mut session = Session::preview();
        session.view = LibraryView::Playlist("night".into());
        let queue = ids_of(&session)
            .into_iter()
            .map(TrackId)
            .collect::<Vec<_>>();
        session.play(queue[0], queue.clone());
        let moved = queue[0];
        assert_eq!(
            session.move_tracks(&[moved], "focus", &session.view.clone()),
            1
        );
        assert!(!session.playlists[0].track_ids.contains(&moved));
        assert!(session.playlists[1].track_ids.contains(&moved));
        assert!(session.current.is_none());
        assert!(!session.playing);
    }

    #[test]
    fn moving_from_liked_unlikes_and_moving_from_all_leaves_only_the_target() {
        let mut session = Session::preview();
        let id = session.tracks.iter().find(|track| track.liked).unwrap().id;
        session.view = LibraryView::Liked;
        assert_eq!(session.move_tracks(&[id], "focus", &LibraryView::Liked), 1);
        assert!(!session.track(id).unwrap().liked);
        assert!(session.playlists[1].track_ids.contains(&id));

        session.view = LibraryView::All;
        let shared = TrackId(4);
        assert!(session.playlists[0].track_ids.contains(&shared));
        assert!(session.playlists[1].track_ids.contains(&shared));
        assert_eq!(
            session.move_tracks(&[shared], "focus", &LibraryView::All),
            1
        );
        assert!(!session.playlists[0].track_ids.contains(&shared));
        assert!(session.playlists[1].track_ids.contains(&shared));
    }

    #[test]
    fn reordering_playlists_is_a_permutation() {
        let mut session = Session::preview();
        let ids: Vec<String> = session
            .playlists
            .iter()
            .map(|playlist| playlist.id.clone())
            .collect();
        assert!(session.reorder_playlists(&[ids[0].clone()], ids.len()));
        assert_eq!(session.playlists.last().unwrap().id, ids[0]);
        assert_eq!(session.playlists.len(), ids.len());
        assert!(!session.reorder_playlists(&[ids[0].clone()], ids.len()));
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
        assert!(!session
            .playlists
            .iter()
            .find(|playlist| playlist.id == "night")
            .unwrap()
            .track_ids
            .contains(&TrackId(0)));
        session.delete_playlist("focus");
        assert!(session
            .playlists
            .iter()
            .all(|playlist| playlist.id != "focus"));
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
        assert_eq!(session.order_mode, OrderMode::Shuffle);
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
        assert_eq!(
            session.upcoming_ids(session.current.unwrap()),
            vec![session.current.unwrap()]
        );

        session.cycle_repeat();
        assert_eq!(session.repeat, RepeatMode::Off);
        session.set_order_mode(OrderMode::Random);
        assert!(session.shuffle);
        assert_eq!(session.order_mode, OrderMode::Random);
        session.toggle_shuffle();
        assert!(!session.shuffle);
        assert_eq!(session.order_mode, OrderMode::Random);
        assert_eq!(
            session.upcoming_ids(TrackId(2)),
            vec![TrackId(2), TrackId(3), TrackId(4), TrackId(5)]
        );
    }

    #[test]
    fn closing_the_session_remembers_the_open_track() {
        let path = std::env::temp_dir().join(format!(
            "muzeeka-playback-{}-session.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE app_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                active_playlist_id TEXT,
                current_file TEXT,
                volume REAL,
                shuffle_enabled INTEGER NOT NULL DEFAULT 0,
                repeat_mode TEXT NOT NULL DEFAULT 'off'
            );",
        )
        .unwrap();

        let tracks = vec![
            Track {
                id: TrackId(3),
                title: "Street".into(),
                artist: "Playaz".into(),
                album: "Remix".into(),
                duration: 180.0,
                liked: false,
                path: r"Z:\Music\Street.mp3".into(),
                cover: None,
                lyrics: None,
            },
            Track {
                id: TrackId(4),
                title: "Next".into(),
                artist: "Playaz".into(),
                album: "Remix".into(),
                duration: 120.0,
                liked: false,
                path: r"Z:\Music\Next.mp3".into(),
                cover: None,
                lyrics: None,
            },
        ];
        let playlists = vec![Playlist {
            id: "mix".into(),
            name: "Mix".into(),
            track_ids: vec![TrackId(3), TrackId(4)],
            cover: None,
            cover_path: None,
            mix_mode: false,
        }];
        let mut session = Session::from_library(LoadedLibrary {
            tracks: tracks.clone(),
            playlists: playlists.clone(),
            view: LibraryView::Playlist("mix".into()),
            current: None,
            position: 0.0,
            volume: 0.5,
            shuffle: false,
            repeat: RepeatMode::Off,
            playing_playlist_id: None,
            was_playing: false,
            db,
        });
        session.play(TrackId(3), vec![TrackId(3), TrackId(4)]);
        session.position = 40.0;
        drop(session);

        let db = Connection::open(&path).unwrap();
        let stored = catalog::load_state(&db).unwrap();
        assert_eq!(stored.current_file.as_deref(), Some(r"Z:\Music\Street.mp3"));
        assert!((stored.position - 40.0).abs() < 1e-6);
        assert!(stored.was_playing);
        assert_eq!(stored.active_playlist_id.as_deref(), Some("mix"));
        assert_eq!(stored.playing_playlist_id.as_deref(), Some("mix"));
        assert!((stored.volume - 0.5).abs() < 1e-3);

        let current = tracks
            .iter()
            .find(|track| stored.current_file.as_deref() == Some(track.path.as_str()))
            .map(|track| track.id);
        let restored = Session::from_library(LoadedLibrary {
            tracks,
            playlists,
            view: LibraryView::Playlist(stored.active_playlist_id.clone().unwrap()),
            current,
            position: stored.position,
            volume: stored.volume,
            shuffle: stored.shuffle,
            repeat: stored.repeat,
            playing_playlist_id: stored.playing_playlist_id.clone(),
            was_playing: stored.was_playing,
            db,
        });
        assert_eq!(restored.current, Some(TrackId(3)));
        assert!((restored.position - 40.0).abs() < 1e-6);
        assert!(restored.playing);
        assert_eq!(restored.queue, vec![TrackId(3), TrackId(4)]);
        drop(restored);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn window_title_uses_artist_and_track() {
        assert_eq!(
            format_window_title("Playaz", "Street"),
            "Playaz - Street | muzeeka"
        );
        assert_eq!(format_window_title("", "Street"), "Street | muzeeka");
        assert_eq!(format_window_title("Playaz", "  "), "Playaz | muzeeka");
        assert_eq!(format_window_title("  ", ""), "muzeeka");
    }

    #[test]
    fn playback_error_replaces_itself_and_expires() {
        let mut session = Session::preview();
        session.note_playback_error("no decoder".into());
        session.note_playback_error("still closed".into());
        assert_eq!(session.playback_error.as_deref(), Some("still closed"));
        assert!(session.playback_error.is_some());
        session.playback_error_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(session.tick(0.1));
        assert!(session.playback_error.is_none());
    }

    #[test]
    fn cue_parent_stays_out_of_search_and_the_split_keeps_its_cover() {
        let cover = std::path::PathBuf::from(r"C:\covers\c-0123456789abcdef-thumb.webp");
        let mut session = Session::empty();
        session.tracks = vec![
            Track {
                id: TrackId(1),
                title: "Full Show".into(),
                artist: "Band".into(),
                album: "Live".into(),
                duration: 3000.0,
                liked: false,
                path: r"Z:\Music\live.flac".into(),
                cover: Some(cover.clone()),
                lyrics: None,
            },
            Track {
                id: TrackId(2),
                title: "Intro".into(),
                artist: "Band".into(),
                album: "Live".into(),
                duration: 120.0,
                liked: false,
                path: r"Z:\Music\live.flac#cue:1".into(),
                cover: None,
                lyrics: None,
            },
            Track {
                id: TrackId(3),
                title: "Verse".into(),
                artist: "Band".into(),
                album: "Live".into(),
                duration: 180.0,
                liked: false,
                path: r"Z:\Music\live.flac#cue:2".into(),
                cover: None,
                lyrics: None,
            },
        ];
        session.playlists = vec![Playlist {
            id: "show".into(),
            name: "Show".into(),
            track_ids: vec![TrackId(1), TrackId(2)],
            cover: None,
            cover_path: None,
            mix_mode: false,
        }];
        session.rebuild_indexes();
        assert_eq!(session.tracks[1].cover, Some(cover.clone()));
        assert_eq!(session.tracks[2].cover, Some(cover));
        let hits = session.search_ids("live");
        assert_eq!(hits, vec![TrackId(2), TrackId(3)]);
        assert!(session
            .visible_indexes("")
            .iter()
            .all(|index| { session.tracks[*index].id != TrackId(1) }));
        session.view = LibraryView::Playlist("show".into());
        let in_playlist = session.visible_indexes("");
        assert!(in_playlist
            .iter()
            .any(|index| session.tracks[*index].id == TrackId(1)));
    }

    #[test]
    fn shuffle_walks_every_track_once_then_stops_and_wraps_on_repeat_all() {
        let mut session = Session::preview();
        let queue: Vec<_> = (0..5).map(TrackId).collect();
        session.play(TrackId(0), queue.clone());
        session.repeat = RepeatMode::Off;
        session.toggle_shuffle();
        let order = session.shuffle_order.clone();
        assert_eq!(order.first().copied(), Some(TrackId(0)));
        assert_eq!(order.len(), 5);
        let mut sorted = order.clone();
        sorted.sort_by_key(|id| id.0);
        assert_eq!(sorted, queue);
        assert_ne!(order, queue);

        session.position = 0.0;
        session.next();
        assert_eq!(session.current, Some(order[1]));
        session.position = 0.0;
        session.prev();
        assert_eq!(session.current, Some(order[0]));
        assert!(session.playing);

        for id in order.iter().skip(1).copied() {
            session.position = 0.0;
            session.next();
            assert_eq!(session.current, Some(id));
            assert!(session.playing);
        }
        session.position = 0.0;
        session.next();
        assert!(!session.playing);
        assert_eq!(session.current, order.last().copied());

        session.repeat = RepeatMode::All;
        session.next();
        assert_eq!(session.current, Some(order[0]));
        assert!(session.playing);
        session.position = 0.0;
        session.prev();
        assert_eq!(session.current, Some(order[0]));
        assert!(!session.playing);

        session.position = 0.0;
        let (has_prev, has_next) = session.can_navigate();
        assert!(!has_prev);
        assert!(has_next);

        session.play(TrackId(2), queue);
        session.repeat = RepeatMode::One;
        session.next();
        assert_eq!(session.current, Some(TrackId(2)));
        assert!(session.playing);
        assert!(session.taskbar_flags().3 || session.repeat == RepeatMode::One);
    }

    #[test]
    fn shuffle_keeps_its_deck_until_the_playlist_changes() {
        let mut session = Session::preview();
        let queue: Vec<_> = (0..5).map(TrackId).collect();
        session.play(TrackId(0), queue.clone());
        session.toggle_shuffle();
        let order = session.shuffle_order.clone();
        assert_eq!(order.first().copied(), Some(TrackId(0)));

        session.play(TrackId(3), queue.clone());
        assert_eq!(session.shuffle_order, order);
        assert_eq!(session.current, Some(TrackId(3)));
        session.repeat = RepeatMode::All;
        session.position = 0.0;
        session.next();
        let after = order.iter().position(|id| *id == TrackId(3)).unwrap() + 1;
        let expected = if after < order.len() {
            order[after]
        } else {
            order[0]
        };
        assert_eq!(session.current, Some(expected));

        session.repeat = RepeatMode::Off;
        let other = vec![TrackId(6), TrackId(7), TrackId(8), TrackId(9)];
        session.play(TrackId(8), other.clone());
        assert_eq!(session.shuffle_order.first().copied(), Some(TrackId(8)));
        assert_eq!(session.shuffle_order.len(), other.len());
        let mut sorted = session.shuffle_order.clone();
        sorted.sort_by_key(|id| id.0);
        assert_eq!(sorted, other);
        assert_ne!(session.shuffle_order, order);
    }

    #[test]
    fn shuffle_random_picks_without_a_deck() {
        let mut session = Session::preview();
        let queue: Vec<_> = (0..4).map(TrackId).collect();
        session.play(TrackId(0), queue.clone());
        session.toggle_shuffle();
        session.set_order_mode(OrderMode::Random);
        assert!(session.shuffle);
        assert_eq!(session.order_mode, OrderMode::Random);
        assert!(session.shuffle_order.is_empty());
        assert_eq!(session.upcoming_ids(TrackId(0)), vec![TrackId(0)]);

        session.position = 30.0;
        session.prev();
        assert_ne!(session.current, Some(TrackId(0)));
        assert_eq!(session.position, 0.0);
        assert!(queue.contains(&session.current.unwrap()));

        let mut seen = std::collections::HashSet::new();
        seen.insert(session.current.unwrap());
        for _ in 0..12 {
            let before = session.current.unwrap();
            session.position = 0.0;
            session.next();
            let now = session.current.unwrap();
            assert_ne!(now, before);
            assert!(queue.contains(&now));
            seen.insert(now);
        }
        assert!(seen.len() > 2, "random keeps visiting the queue");
        assert!(session.shuffle_order.is_empty());

        session.repeat = RepeatMode::One;
        let held = session.current;
        session.next();
        assert_eq!(session.current, held);

        assert_eq!(OrderMode::from_setting(Some("random")), OrderMode::Random);
        assert_eq!(OrderMode::from_setting(Some("smart")), OrderMode::Shuffle);
        assert_eq!(OrderMode::from_setting(Some("shuffle")), OrderMode::Shuffle);
        assert_eq!(OrderMode::from_setting(None), OrderMode::Shuffle);
    }
}
