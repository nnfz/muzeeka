use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    actions, canvas, div, prelude::*, px, relative, size, AnyElement, App, Bounds, Context,
    FocusHandle, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Render,
    SharedString, Styled, TitlebarOptions, Window, WindowBounds, WindowControlArea, WindowHandle,
    WindowOptions,
};
use muzeeka::audio::cue::{self, is_cue_track_path};
use muzeeka::audio::tag_table::{self, TagTableRow};
use muzeeka::audio::tech::{self, AudioTechInfo};

use crate::ui::catalog::{self, LibraryFields};
use crate::ui::icons::{self, icon};
use crate::ui::menu::{self, file_on_disk};
use crate::ui::paths::is_stream_url;
use crate::ui::session::{self, Session, TrackId};
use crate::ui::text_field::TextField;
use crate::ui::theme;

actions!(properties_window, [SaveProperties, CloseProperties]);

const RATE_PRESETS: [f32; 5] = [0.75, 0.85, 1.0, 1.25, 1.5];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Metadata,
    Details,
    Lyrics,
    Muzeeka,
}

impl Section {
    const ALL: [Self; 4] = [Self::Metadata, Self::Details, Self::Lyrics, Self::Muzeeka];

    fn label(self) -> &'static str {
        match self {
            Self::Metadata => "Metadata",
            Self::Details => "Details",
            Self::Lyrics => "Lyrics",
            Self::Muzeeka => "Muzeeka",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchSlot {
    Title,
    Artist,
    Album,
}

#[derive(Clone)]
struct Snap {
    id: TrackId,
    path: String,
    title: String,
    artist: String,
    album: String,
    duration: f64,
}

pub struct PropertiesWindow {
    session: gpui::Entity<Session>,
    focus: FocusHandle,
    use_ui_font: bool,
    snaps: Vec<Snap>,
    section: Section,
    armed: Option<SharedString>,
    rows: Vec<TagTableRow>,
    mixed: HashSet<String>,
    changed: HashSet<String>,
    fields: HashMap<String, gpui::Entity<TextField>>,
    filter: gpui::Entity<TextField>,
    lyrics_field: gpui::Entity<TextField>,
    search_title: gpui::Entity<TextField>,
    search_artist: gpui::Entity<TextField>,
    search_album: gpui::Entity<TextField>,
    bpm_field: gpui::Entity<TextField>,
    search_menu: Option<SearchSlot>,
    search_open: bool,
    tech: AudioTechInfo,
    loading: bool,
    saving: bool,
    cover_busy: bool,
    lyrics_busy: bool,
    bpm_busy: bool,
    bpm_detecting: bool,
    video_busy: bool,
    load_gen: u64,
    status: Option<String>,
    error: Option<String>,
    lyrics_status: Option<String>,
    lyrics_error: Option<String>,
    lyrics_dirty: bool,
    lyrics_base: String,
    rate_override: Option<f32>,
    rate_draft: f32,
    rate_drag: bool,
    rate_bounds: Option<Bounds<Pixels>>,
    bpm_value: Option<f32>,
    tap_times: Vec<Instant>,
    tap_gen: u64,
    video_path: Option<String>,
    extra_status: Option<String>,
    extra_error: Option<String>,
    shown_title: String,
    pending_close: bool,
}

impl PropertiesWindow {
    pub fn open(
        session: gpui::Entity<Session>,
        ids: Vec<TrackId>,
        use_ui_font: bool,
        existing: Option<WindowHandle<Self>>,
        cx: &mut App,
    ) -> Option<WindowHandle<Self>> {
        if let Some(handle) = existing {
            if handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
            {
                return Some(handle);
            }
        }
        let key = {
            let library = session.read(cx);
            let paths = ids
                .iter()
                .filter_map(|id| library.track(*id).map(|track| track.path.clone()))
                .collect::<Vec<_>>();
            if paths.is_empty() {
                return None;
            }
            properties_key(&paths)
        };
        let bounds = cascade_bounds(&key, cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Properties".into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                window_min_size: Some(size(px(720.), px(560.))),
                is_minimizable: false,
                ..Default::default()
            },
            move |_window, cx| cx.new(|cx| Self::new(session, ids, use_ui_font, cx)),
        )
        .ok()
    }

    fn new(
        session: gpui::Entity<Session>,
        ids: Vec<TrackId>,
        use_ui_font: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let snaps = {
            let library = session.read(cx);
            ids.iter()
                .filter_map(|id| {
                    library.track(*id).map(|track| Snap {
                        id: *id,
                        path: track.path.clone(),
                        title: track.title.clone(),
                        artist: track.artist.clone(),
                        album: track.album.clone(),
                        duration: track.duration,
                    })
                })
                .collect::<Vec<_>>()
        };
        let global = session.read(cx).rack.playback_rate;
        let host = cx.entity().clone();
        let mut window = Self {
            session,
            focus: cx.focus_handle(),
            use_ui_font,
            snaps,
            section: Section::Metadata,
            armed: None,
            rows: Vec::new(),
            mixed: HashSet::new(),
            changed: HashSet::new(),
            fields: HashMap::new(),
            filter: text_field(
                cx,
                "properties-filter",
                "Filter name or value",
                host.clone(),
                false,
                watch_text(host.clone()),
            ),
            lyrics_field: text_field(
                cx,
                "properties-lyrics",
                "Paste or edit lyrics (TTML / plain text)…",
                host.clone(),
                true,
                watch_lyrics(host.clone()),
            ),
            search_title: text_field(
                cx,
                "properties-search-title",
                "Title override",
                host.clone(),
                false,
                watch_text(host.clone()),
            ),
            search_artist: text_field(
                cx,
                "properties-search-artist",
                "Artist override",
                host.clone(),
                false,
                watch_text(host.clone()),
            ),
            search_album: text_field(
                cx,
                "properties-search-album",
                "Album override",
                host.clone(),
                false,
                watch_text(host.clone()),
            ),
            bpm_field: text_field(
                cx,
                "properties-bpm",
                "BPM",
                host.clone(),
                false,
                ignore_text,
            ),
            search_menu: None,
            search_open: false,
            tech: AudioTechInfo::default(),
            loading: true,
            saving: false,
            cover_busy: false,
            lyrics_busy: false,
            bpm_busy: false,
            bpm_detecting: false,
            video_busy: false,
            load_gen: 0,
            status: None,
            error: None,
            lyrics_status: None,
            lyrics_error: None,
            lyrics_dirty: false,
            lyrics_base: String::new(),
            rate_override: None,
            rate_draft: global,
            rate_drag: false,
            rate_bounds: None,
            bpm_value: None,
            tap_times: Vec::new(),
            tap_gen: 0,
            video_path: None,
            extra_status: None,
            extra_error: None,
            shown_title: String::new(),
            pending_close: false,
        };
        window.bind_bpm(cx);
        window.begin_load(cx);
        window
    }

    fn bind_bpm(&mut self, cx: &mut Context<Self>) {
        let host = cx.entity().clone();
        let submit_host = host.clone();
        self.bpm_field.update(cx, |field, _cx| {
            field.set_on_submit(move |_text, window, app| {
                let host = submit_host.clone();
                window.defer(app, move |_window, app| {
                    host.update(app, |view, cx| view.write_bpm(cx));
                });
            });
            field.set_on_escape(escape_cb(host));
        });
    }

    fn arm(&mut self, id: impl Into<SharedString>) {
        self.armed = Some(id.into());
    }

    fn armed(&self, id: &str) -> bool {
        self.armed.as_deref() == Some(id)
    }

    fn batch(&self) -> bool {
        self.snaps.len() > 1
    }

    fn primary(&self) -> Option<&Snap> {
        self.snaps.first()
    }

    fn begin_load(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.load_gen = self.load_gen.wrapping_add(1);
        let gen = self.load_gen;
        let snaps = self.snaps.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { load_blocking(&snaps) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.load_gen == gen {
                    view.apply_load(loaded, cx);
                }
            });
        })
        .detach();
    }

    fn apply_load(&mut self, loaded: LoadResult, cx: &mut Context<Self>) {
        self.loading = false;
        self.rows = loaded.rows;
        self.mixed = loaded.mixed;
        self.changed.clear();
        self.tech = loaded.tech;
        self.error = loaded.error;
        self.lyrics_base = loaded.lyrics.clone();
        self.lyrics_dirty = false;
        self.rate_override = loaded.rate;
        self.rate_draft = loaded
            .rate
            .unwrap_or_else(|| self.session.read(cx).rack.playback_rate);
        self.bpm_value = loaded.bpm_value;
        self.video_path = loaded.video;
        self.lyrics_field
            .update(cx, |field, cx| field.replace_text(&loaded.lyrics, cx));
        self.bpm_field
            .update(cx, |field, cx| field.replace_text(&loaded.bpm_text, cx));
        self.rebuild_fields(cx);
        cx.notify();
    }

    fn rebuild_fields(&mut self, cx: &mut Context<Self>) {
        self.fields.clear();
        let host = cx.entity().clone();
        for row in self.rows.clone() {
            if row.read_only {
                continue;
            }
            let field = row_field(cx, &row, self.mixed.contains(&row.id), host.clone());
            field.update(cx, |field, cx| field.replace_text(&row.value, cx));
            self.fields.insert(row.id.clone(), field);
        }
    }

    fn note_row(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter_mut().find(|row| row.id == id) else {
            return;
        };
        if row.value == value {
            return;
        }
        row.value = value.to_string();
        self.changed.insert(id.to_string());
        self.status = None;
        self.error = None;
        cx.notify();
    }

    fn note_lyrics(&mut self, value: &str, cx: &mut Context<Self>) {
        let dirty = value != self.lyrics_base;
        if dirty == self.lyrics_dirty && self.lyrics_error.is_none() {
            return;
        }
        self.lyrics_dirty = dirty;
        if dirty {
            self.lyrics_status = None;
            self.lyrics_error = None;
        }
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_menu.take().is_some() {
            cx.notify();
            return;
        }
        window.remove_window();
    }

    fn save_shortcut(&mut self, cx: &mut Context<Self>) {
        if self.section == Section::Lyrics {
            self.save_lyrics(cx);
        } else {
            self.save_tags(cx);
        }
    }

    fn save_tags(&mut self, cx: &mut Context<Self>) {
        if self.saving || self.loading || self.snaps.is_empty() {
            return;
        }
        self.saving = true;
        self.error = None;
        self.status = None;
        let rows = self.rows.clone();
        let changed = self.changed.clone();
        let snaps = self.snaps.clone();
        let batch = self.batch();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { save_tags_blocking(&snaps, &rows, &changed, batch) })
                .await;
            let _ = this.update(cx, |view, cx| view.finish_save(outcome, cx));
        })
        .detach();
        cx.notify();
    }

    fn finish_save(&mut self, outcome: SaveOutcome, cx: &mut Context<Self>) {
        self.saving = false;
        for saved in &outcome.saved {
            self.session.update(cx, |session, cx| {
                session.apply_saved_tags(
                    saved.id,
                    saved.title.clone(),
                    saved.artist.clone(),
                    saved.album.clone(),
                );
                cx.notify();
            });
            if let Some(snap) = self.snaps.iter_mut().find(|snap| snap.id == saved.id) {
                snap.title = saved.title.clone();
                snap.artist = saved.artist.clone();
                snap.album = saved.album.clone();
            }
        }
        if outcome.error.is_none() {
            self.changed.clear();
            self.status = Some(outcome.message);
            self.rows = outcome.rows;
            self.mixed = outcome.mixed;
            self.rebuild_fields(cx);
        } else {
            self.error = outcome.error;
        }
        cx.notify();
    }

    fn change_cover(&mut self, cx: &mut Context<Self>) {
        if self.batch() || self.cover_busy {
            return;
        }
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        if is_stream_url(&snap.path) {
            self.error = Some("Streams have no file to embed a cover into".into());
            cx.notify();
            return;
        }
        let picked = match menu::pick_image() {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let bytes = match std::fs::read(&picked) {
            Ok(bytes) if !bytes.is_empty() => bytes,
            Ok(_) => {
                self.error = Some("Image file is empty".into());
                cx.notify();
                return;
            }
            Err(error) => {
                self.error = Some(format!("Could not read the image: {error}"));
                cx.notify();
                return;
            }
        };
        let mime = mime_for(&picked).map(str::to_string);
        let siblings = self.session.read(cx).ids_for_audio_file(&snap.path);
        self.cover_busy = true;
        self.error = None;
        self.status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(
                    async move { write_cover_blocking(&snap, &bytes, mime.as_deref(), &siblings) },
                )
                .await;
            let _ = this.update(cx, |view, cx| {
                view.cover_busy = false;
                match result {
                    Ok((message, cover, ids)) => {
                        view.status = Some(message);
                        view.session.update(cx, |session, cx| {
                            session.apply_cover_paths(&ids, Some(cover));
                            cx.notify();
                        });
                    }
                    Err(error) => view.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_lyrics(&mut self, cx: &mut Context<Self>) {
        if self.lyrics_busy || self.batch() && false {
            return;
        }
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        let text = self.lyrics_field.read(cx).content.to_string();
        if text.trim().is_empty() {
            self.lyrics_error = Some("Text is empty — use Clear to remove lyrics".into());
            self.lyrics_status = None;
            cx.notify();
            return;
        }
        self.lyrics_busy = true;
        self.lyrics_error = None;
        self.lyrics_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::ui::lyrics_store::save_text(
                        &snap.title,
                        &snap.artist,
                        Some(snap.album.as_str()),
                        duration_secs(snap.duration),
                        &text,
                    )
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.lyrics_busy = false;
                match result {
                    Ok(()) => {
                        view.lyrics_base = view.lyrics_field.read(cx).content.to_string();
                        view.lyrics_dirty = false;
                        view.lyrics_status = Some("Lyrics saved".into());
                        view.session.update(cx, |session, cx| {
                            session.refresh_lyrics(snap.id);
                            cx.notify();
                        });
                    }
                    Err(error) => view.lyrics_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn find_lyrics(&mut self, cx: &mut Context<Self>) {
        if self.lyrics_busy {
            return;
        }
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        let search_title = blank_to_none(&self.search_title.read(cx).content);
        let search_artist = blank_to_none(&self.search_artist.read(cx).content);
        let search_album = blank_to_none(&self.search_album.read(cx).content);
        self.lyrics_busy = true;
        self.lyrics_error = None;
        self.lyrics_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::ui::lyrics_store::refetch(
                        &snap.title,
                        &snap.artist,
                        Some(snap.album.as_str()),
                        duration_secs(snap.duration),
                        search_title.as_deref(),
                        search_artist.as_deref(),
                        search_album.as_deref(),
                    )
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.lyrics_busy = false;
                match result {
                    Ok(Some(text)) => {
                        view.lyrics_base = text.clone();
                        view.lyrics_dirty = false;
                        view.lyrics_field
                            .update(cx, |field, cx| field.replace_text(&text, cx));
                        view.lyrics_status = Some("Lyrics found".into());
                        view.session.update(cx, |session, cx| {
                            session.refresh_lyrics(snap.id);
                            cx.notify();
                        });
                    }
                    Ok(None) => view.lyrics_status = Some("No lyrics found".into()),
                    Err(error) => view.lyrics_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn import_lyrics(&mut self, cx: &mut Context<Self>) {
        if self.lyrics_busy {
            return;
        }
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        let picked = match menu::pick_file("Lyrics", "Lyrics|*.ttml;*.xml;*.lrc;*.txt") {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(error) => {
                self.lyrics_error = Some(error);
                cx.notify();
                return;
            }
        };
        self.lyrics_busy = true;
        self.lyrics_error = None;
        self.lyrics_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::ui::lyrics_store::import_file(
                        &snap.title,
                        &snap.artist,
                        Some(snap.album.as_str()),
                        duration_secs(snap.duration),
                        &picked,
                    )?;
                    crate::ui::lyrics_store::read_cached(
                        &snap.title,
                        &snap.artist,
                        Some(snap.album.as_str()),
                        duration_secs(snap.duration),
                    )
                    .ok_or_else(|| "Imported lyrics could not be read back".to_string())
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.lyrics_busy = false;
                match result {
                    Ok(text) => {
                        view.lyrics_base = text.clone();
                        view.lyrics_dirty = false;
                        view.lyrics_field
                            .update(cx, |field, cx| field.replace_text(&text, cx));
                        view.lyrics_status = Some("Lyrics imported".into());
                        view.session.update(cx, |session, cx| {
                            session.refresh_lyrics(snap.id);
                            cx.notify();
                        });
                    }
                    Err(error) => view.lyrics_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn clear_lyrics(&mut self, cx: &mut Context<Self>) {
        if self.lyrics_busy {
            return;
        }
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        self.lyrics_busy = true;
        self.lyrics_error = None;
        self.lyrics_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    crate::ui::lyrics_store::clear(
                        &snap.title,
                        &snap.artist,
                        Some(snap.album.as_str()),
                        duration_secs(snap.duration),
                    )
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.lyrics_busy = false;
                match result {
                    Ok(()) => {
                        view.lyrics_base.clear();
                        view.lyrics_dirty = false;
                        view.lyrics_field
                            .update(cx, |field, cx| field.replace_text("", cx));
                        view.lyrics_status = Some("Lyrics cleared".into());
                        view.session.update(cx, |session, cx| {
                            session.refresh_lyrics(snap.id);
                            cx.notify();
                        });
                    }
                    Err(error) => view.lyrics_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn commit_rate(&mut self, rate: Option<f32>, cx: &mut Context<Self>) {
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        let global = self.session.read(cx).rack.playback_rate;
        let stored = rate.map(|rate| rate.clamp(0.25, 2.0));
        if let Err(error) = (|| {
            let db = catalog::open_library()?;
            catalog::set_track_playback_rate(&db, &snap.path, stored)
        })() {
            self.extra_error = Some(error);
            self.extra_status = None;
            cx.notify();
            return;
        }
        self.rate_override = stored;
        self.rate_draft = stored.unwrap_or(global);
        self.extra_error = None;
        self.extra_status = Some(match stored {
            Some(rate) => format!("Track speed set to {rate:.2}×"),
            None => "Using global speed from Settings".into(),
        });
        let applied = stored.unwrap_or(global);
        self.session.update(cx, |session, cx| {
            if session.plays_path(&snap.path) {
                session.push_engine_rate(applied, false);
            }
            cx.notify();
        });
        cx.notify();
    }

    fn rate_at(&self, x: Pixels) -> f32 {
        let Some(bounds) = self.rate_bounds else {
            return self.rate_draft;
        };
        let width = bounds.size.width.as_f32().max(1.0);
        let frac = ((x.as_f32() - bounds.origin.x.as_f32()) / width).clamp(0.0, 1.0);
        let rate = 0.25 + frac * 1.75;
        ((rate * 100.0).round() / 100.0).clamp(0.25, 2.0)
    }

    fn drag_rate(&mut self, x: Pixels, cx: &mut Context<Self>) {
        if !self.rate_drag {
            return;
        }
        self.rate_draft = self.rate_at(x);
        self.extra_status = None;
        cx.notify();
    }

    fn end_rate_drag(&mut self, cx: &mut Context<Self>) {
        if !self.rate_drag {
            return;
        }
        self.rate_drag = false;
        self.commit_rate(Some(self.rate_draft), cx);
    }

    fn tap_bpm(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self
            .tap_times
            .last()
            .is_some_and(|last| now.duration_since(*last) > Duration::from_millis(2500))
        {
            self.tap_times.clear();
        }
        self.tap_times.push(now);
        if self.tap_times.len() > 12 {
            let extra = self.tap_times.len() - 12;
            self.tap_times.drain(0..extra);
        }
        if let Some(bpm) = median_bpm(&self.tap_times) {
            let text = format_bpm(bpm);
            self.bpm_field
                .update(cx, |field, cx| field.replace_text(&text, cx));
        }
        self.tap_gen = self.tap_gen.wrapping_add(1);
        let gen = self.tap_gen;
        self.extra_error = None;
        self.extra_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(2500))
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.tap_gen == gen {
                    view.tap_times.clear();
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn detect_bpm(&mut self, cx: &mut Context<Self>) {
        if self.bpm_detecting {
            return;
        }
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        if is_stream_url(&snap.path) {
            self.extra_error = Some("Streams have no file to detect BPM from".into());
            cx.notify();
            return;
        }
        let path = match tag_write_path(&snap.path) {
            Ok(path) => path.to_string_lossy().to_string(),
            Err(error) => {
                self.extra_error = Some(error);
                cx.notify();
                return;
            }
        };
        let player = self
            .session
            .read(cx)
            .engine
            .as_ref()
            .map(|engine| engine.clone_player());
        let Some(player) = player else {
            self.extra_error = Some("Audio output is unavailable".into());
            cx.notify();
            return;
        };
        self.bpm_detecting = true;
        self.extra_error = None;
        self.extra_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { muzeeka::audio::bpm::detect_bpm_for_path(&player, &path) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.bpm_detecting = false;
                match result {
                    Ok(bpm) => {
                        let text = format_bpm(bpm);
                        view.bpm_field
                            .update(cx, |field, cx| field.replace_text(&text, cx));
                        view.extra_status = Some(format!("Detected {text} BPM"));
                    }
                    Err(error) => view.extra_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn write_bpm(&mut self, cx: &mut Context<Self>) {
        if self.bpm_busy || self.bpm_detecting {
            return;
        }
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        let draft = self.bpm_field.read(cx).content.to_string();
        let Some(bpm) = parse_bpm(&draft) else {
            self.extra_error = Some("Enter a BPM between 1 and 999".into());
            cx.notify();
            return;
        };
        if is_stream_url(&snap.path) {
            self.extra_error = Some("Streams have no file to write BPM into".into());
            cx.notify();
            return;
        }
        let path = match tag_write_path(&snap.path) {
            Ok(path) => path,
            Err(error) => {
                self.extra_error = Some(error);
                cx.notify();
                return;
            }
        };
        self.bpm_busy = true;
        self.extra_error = None;
        self.extra_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { tag_table::write_bpm(&path, bpm) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.bpm_busy = false;
                match result {
                    Ok(()) => {
                        view.bpm_value = Some(bpm);
                        let text = format_bpm(bpm);
                        view.set_row_text("Bpm", &text, cx);
                        view.set_row_text("IntegerBpm", &format!("{}", bpm.round() as u32), cx);
                        view.extra_status = Some(format!("Wrote {text} BPM into the file"));
                    }
                    Err(error) => view.extra_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_row_text(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.iter_mut().find(|row| row.id == id) {
            row.value = value.to_string();
        }
        if let Some(field) = self.fields.get(id) {
            let value = value.to_string();
            field.update(cx, |field, cx| field.replace_text(&value, cx));
        }
    }

    fn pick_video(&mut self, cx: &mut Context<Self>) {
        if self.video_busy {
            return;
        }
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        let picked = match menu::pick_file("Video", "Video|*.mp4;*.webm;*.mkv;*.mov;*.m4v") {
            Ok(Some(path)) => path,
            Ok(None) => return,
            Err(error) => {
                self.extra_error = Some(error);
                cx.notify();
                return;
            }
        };
        self.store_video(snap, Some(picked.to_string_lossy().to_string()), cx);
    }

    fn clear_video(&mut self, cx: &mut Context<Self>) {
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        self.store_video(snap, None, cx);
    }

    fn store_video(&mut self, snap: Snap, path: Option<String>, cx: &mut Context<Self>) {
        self.video_busy = true;
        self.extra_error = None;
        self.extra_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let db = catalog::open_library()?;
                    catalog::set_track_video_bg(&db, &snap.path, path.as_deref())?;
                    Ok::<Option<String>, String>(path)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.video_busy = false;
                match result {
                    Ok(path) => {
                        view.video_path = path.clone();
                        view.extra_status = Some(if path.is_some() {
                            "Video background set".into()
                        } else {
                            "Video background removed".into()
                        });
                    }
                    Err(error) => view.extra_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn search_video(&mut self, cx: &mut Context<Self>) {
        if self.video_busy {
            return;
        }
        let Some(snap) = self.primary().cloned() else {
            return;
        };
        if snap.title.trim().is_empty() || snap.artist.trim().is_empty() {
            self.extra_error = Some("Track needs an artist and a title to search YouTube".into());
            cx.notify();
            return;
        }
        self.video_busy = true;
        self.extra_error = None;
        self.extra_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let output = crate::ui::ytdlp::video_bg_output(&snap.path);
                    crate::ui::ytdlp::download_video_bg_clip(&snap.artist, &snap.title, &output)?;
                    let stored = output.to_string_lossy().to_string();
                    let db = catalog::open_library()?;
                    catalog::set_track_video_bg(&db, &snap.path, Some(&stored))?;
                    Ok::<String, String>(stored)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.video_busy = false;
                match result {
                    Ok(path) => {
                        view.video_path = Some(path);
                        view.extra_status = Some("Video downloaded from YouTube".into());
                    }
                    Err(error) => view.extra_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn row_value(&self, id: &str) -> String {
        self.rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.value.clone())
            .unwrap_or_default()
    }

    fn button(
        &mut self,
        id: &'static str,
        label: String,
        enabled: bool,
        primary: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let arm = SharedString::from(id);
        let release = SharedString::from(id);
        div()
            .id(id)
            .h(px(28.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded_md()
            .text_size(px(12.))
            .cursor_pointer()
            .text_color(if enabled {
                theme::text()
            } else {
                theme::text_muted()
            })
            .bg(if primary && enabled {
                theme::accent_soft()
            } else {
                theme::bg_elevated()
            })
            .hover(move |style| {
                if enabled {
                    style.bg(if primary {
                        theme::accent_soft()
                    } else {
                        theme::hover()
                    })
                } else {
                    style
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if enabled {
                        this.arm(arm.clone());
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if !enabled || !this.armed(&release) {
                        return;
                    }
                    this.armed = None;
                    action(this, cx);
                }),
            )
            .child(label)
            .into_any_element()
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let title = if self.snaps.len() > 1 {
            format!("Properties — {} tracks", self.snaps.len())
        } else {
            "Properties".to_string()
        };
        div()
            .flex()
            .items_center()
            .h(px(50.))
            .px(px(12.))
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .text_size(px(13.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .id("properties-close")
                    .occlude()
                    .window_control_area(WindowControlArea::Close)
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded_md()
                    .cursor_pointer()
                    .child(icon(icons::CLOSE, theme::text_secondary(), px(10.)))
                    .hover(|style| style.bg(theme::danger()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseDownEvent, _, _| {
                            this.arm("properties-close");
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseUpEvent, window, _cx| {
                            if this.armed("properties-close") {
                                window.remove_window();
                            }
                        }),
                    ),
            )
            .into_any_element()
    }

    fn section_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let badge = self.rate_override;
        div()
            .w(px(200.))
            .h_full()
            .rounded_md()
            .bg(theme::bg_surface())
            .py(px(8.))
            .children(Section::ALL.map(|section| {
                let active = self.section == section;
                let id = SharedString::from(format!("properties-nav-{}", section.label()));
                let arm = id.clone();
                let release = id.clone();
                div()
                    .id(id)
                    .mx(px(6.))
                    .h(px(34.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(if active {
                        theme::text()
                    } else {
                        theme::text_secondary()
                    })
                    .when(active, |row| row.bg(theme::accent_soft()))
                    .hover(move |style| {
                        style.bg(if active {
                            theme::accent_soft()
                        } else {
                            theme::hover()
                        })
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, _, _| {
                            this.arm(arm.clone());
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                            if !this.armed(&release) {
                                return;
                            }
                            this.section = section;
                            this.search_menu = None;
                            cx.notify();
                        }),
                    )
                    .child(section.label())
                    .when(section == Section::Muzeeka, |row| {
                        row.when_some(badge, |row, rate| {
                            row.child(
                                div()
                                    .ml_auto()
                                    .text_size(px(11.))
                                    .text_color(theme::accent())
                                    .child(format!("{rate:.2}×")),
                            )
                        })
                    })
            }))
            .into_any_element()
    }

    fn body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let section = self.section;
        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .rounded_md()
            .bg(theme::bg_surface())
            .id("properties-body")
            .overflow_y_scroll()
            .p(px(16.))
            .child(match section {
                Section::Metadata => self.metadata(cx),
                Section::Details => self.details(cx),
                Section::Lyrics => self.lyrics(cx),
                Section::Muzeeka => self.muzeeka(cx),
            })
            .into_any_element()
    }

    fn metadata(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.primary().cloned();
        let filter = self.filter.read(cx).content.to_lowercase();
        let filled = self
            .rows
            .iter()
            .filter(|row| !row.value.trim().is_empty())
            .count();
        let cue = snap
            .as_ref()
            .is_some_and(|snap| is_cue_track_path(&snap.path));
        let stream = snap.as_ref().is_some_and(|snap| is_stream_url(&snap.path));
        let cover = snap.as_ref().and_then(|snap| {
            self.session
                .read(cx)
                .track(snap.id)
                .and_then(|track| track.cover.clone())
        });
        let tech_line = tech_summary(
            &self.tech,
            snap.as_ref().map(|snap| snap.duration).unwrap_or(0.0),
            snap.as_ref().map(|snap| snap.path.as_str()).unwrap_or(""),
        );
        let rows = self.rows.clone();
        let mut list = Vec::new();
        for row in &rows {
            if !filter.is_empty()
                && !row.name.to_lowercase().contains(&filter)
                && !row.value.to_lowercase().contains(&filter)
            {
                continue;
            }
            list.push(self.tag_row(row, cx));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(heading(
                "Metadata",
                if self.batch() {
                    format!(
                        "Editing {} tracks. Mixed values stay unless you change them. Speed, BPM, lyrics, and video follow the first track.",
                        self.snaps.len()
                    )
                } else if stream {
                    "Station name and artist are saved in the library. There is no file to tag.".into()
                } else {
                    "Tag table. Empty a field and save to remove it. Ctrl+S saves.".into()
                },
            ))
            .child(
                div()
                    .flex()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(88.))
                            .flex_shrink_0()
                            .rounded_md()
                            .overflow_hidden()
                            .child(crate::ui::cover::cover_art(cover.as_deref(), 88.)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme::text_muted())
                                    .child(snap.as_ref().map(|snap| file_on_disk(&snap.path).to_string()).unwrap_or_default()),
                            )
                            .when(cue, |col| {
                                col.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(theme::text_muted())
                                        .child(format!(
                                            "CUE image {}",
                                            snap.as_ref()
                                                .and_then(|snap| tag_write_path(&snap.path).ok())
                                                .map(|path| path.display().to_string())
                                                .unwrap_or_default()
                                        )),
                                )
                            })
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .child(tech_line),
                            )
                            .child(self.button(
                                "properties-cover",
                                if self.cover_busy {
                                    "Embedding…".into()
                                } else {
                                    "Change cover…".into()
                                },
                                !self.batch() && !self.cover_busy && !stream && !self.loading,
                                false,
                                cx,
                                |this, cx| this.change_cover(cx),
                            )),
                    ),
            )
            .child(
                div()
                    .h(px(32.))
                    .px(px(8.))
                    .rounded_md()
                    .bg(theme::bg_deep())
                    .border_1()
                    .border_color(theme::border())
                    .child(self.filter.clone()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child(format!("{filled} filled")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .bg(theme::bg_deep())
                    .children(list),
            )
            .child(self.status_bar(
                self.error.clone(),
                self.status.clone(),
                self.changed.is_empty().then_some("").unwrap_or("Unsaved changes"),
                cx,
                true,
            ))
            .into_any_element()
    }

    fn tag_row(&mut self, row: &TagTableRow, _cx: &mut Context<Self>) -> AnyElement {
        let multiline = is_multiline_field(&row.id);
        let value = if row.read_only {
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .child(if row.value.is_empty() {
                    "—".to_string()
                } else {
                    row.value.clone()
                })
                .into_any_element()
        } else if let Some(field) = self.fields.get(&row.id).cloned() {
            div()
                .flex_1()
                .min_w(px(0.))
                .h(px(if multiline { 52. } else { 28. }))
                .px(px(6.))
                .rounded_md()
                .bg(theme::bg_surface())
                .child(field)
                .into_any_element()
        } else {
            div().flex_1().into_any_element()
        };
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(10.))
            .py(px(6.))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .w(px(160.))
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .text_color(theme::text_secondary())
                    .child(row.name.clone()),
            )
            .child(value)
            .into_any_element()
    }

    fn details(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let snap = self.primary().cloned();
        let groups = detail_groups(&self.tech, snap.as_ref());
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(heading(
                "Details",
                "File location, stream properties, and playback statistics.",
            ))
            .children(groups.into_iter().map(|(title, rows)| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .pb(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::text_secondary())
                            .child(title),
                    )
                    .children(rows.into_iter().map(|(label, value)| {
                        div()
                            .flex()
                            .gap(px(12.))
                            .py(px(3.))
                            .child(
                                div()
                                    .w(px(140.))
                                    .flex_shrink_0()
                                    .text_size(px(12.))
                                    .text_color(theme::text_muted())
                                    .child(label),
                            )
                            .child(div().flex_1().min_w(px(0.)).text_size(px(12.)).child(value))
                    }))
                    .into_any_element()
            }))
            .child(self.status_bar(None, None, "", cx, false))
            .into_any_element()
    }

    fn lyrics(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.lyrics_busy || self.loading;
        let snap = self.primary().cloned();
        let suggestions = self.suggestion_lists();
        let menu = self.search_menu;
        let dirty_label = if self.lyrics_dirty {
            "Unsaved lyrics changes"
        } else {
            ""
        };
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(heading(
                "Lyrics",
                "Find, import, edit, or clear lyrics for this track (TTML or plain text).",
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(self.button(
                        "lyrics-find",
                        if busy {
                            "Searching…".into()
                        } else {
                            "Find".into()
                        },
                        !busy,
                        false,
                        cx,
                        |this, cx| this.find_lyrics(cx),
                    ))
                    .child(self.button(
                        "lyrics-import",
                        "Import…".into(),
                        !busy,
                        false,
                        cx,
                        |this, cx| this.import_lyrics(cx),
                    ))
                    .child(self.button(
                        "lyrics-clear",
                        "Clear".into(),
                        !busy,
                        false,
                        cx,
                        |this, cx| this.clear_lyrics(cx),
                    ))
                    .child(self.button(
                        "lyrics-save",
                        if busy {
                            "Saving…".into()
                        } else {
                            "Save".into()
                        },
                        !busy,
                        true,
                        cx,
                        |this, cx| this.save_lyrics(cx),
                    ))
                    .child(self.button(
                        "lyrics-terms",
                        if self.search_terms_edited(cx) {
                            "Search terms •".into()
                        } else {
                            "Search terms".into()
                        },
                        true,
                        self.search_open,
                        cx,
                        |this, cx| {
                            this.search_open = !this.search_open;
                            this.search_menu = None;
                            cx.notify();
                        },
                    )),
            )
            .when(self.search_open, |col| {
                col.child(self.search_terms(suggestions, menu, cx))
            })
            .child(
                div()
                    .h(px(220.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded_md()
                    .bg(theme::bg_deep())
                    .border_1()
                    .border_color(theme::border())
                    .child(self.lyrics_field.clone()),
            )
            .child(self.status_bar(
                self.lyrics_error.clone(),
                self.lyrics_status.clone(),
                dirty_label,
                cx,
                false,
            ))
            .when(snap.is_none(), |col| col)
            .into_any_element()
    }

    fn search_terms_edited(&self, cx: &App) -> bool {
        !self.search_title.read(cx).content.trim().is_empty()
            || !self.search_artist.read(cx).content.trim().is_empty()
            || !self.search_album.read(cx).content.trim().is_empty()
    }

    fn search_terms(
        &mut self,
        suggestions: (Vec<String>, Vec<String>, Vec<String>),
        menu: Option<SearchSlot>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().text_size(px(12.)).text_color(theme::text_muted()).child(
                "Overrides change the search only. Saved lyrics stay under this track's own tags.",
            ))
            .child(self.search_line("Title", SearchSlot::Title, self.search_title.clone(), &suggestions.0, menu, cx))
            .child(self.search_line("Artist", SearchSlot::Artist, self.search_artist.clone(), &suggestions.1, menu, cx))
            .child(self.search_line("Album", SearchSlot::Album, self.search_album.clone(), &suggestions.2, menu, cx))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(self.button(
                        "lyrics-reset",
                        "Reset".into(),
                        self.search_terms_edited(cx) && !self.lyrics_busy,
                        false,
                        cx,
                        |this, cx| {
                            this.search_title.update(cx, |field, cx| field.clear(cx));
                            this.search_artist.update(cx, |field, cx| field.clear(cx));
                            this.search_album.update(cx, |field, cx| field.clear(cx));
                            this.search_menu = None;
                            cx.notify();
                        },
                    ))
                    .child(self.button(
                        "lyrics-search",
                        if self.lyrics_busy { "Searching…".into() } else { "Search".into() },
                        !self.lyrics_busy && !self.loading,
                        true,
                        cx,
                        |this, cx| this.find_lyrics(cx),
                    )),
            )
            .into_any_element()
    }

    fn search_line(
        &mut self,
        label: &'static str,
        slot: SearchSlot,
        field: gpui::Entity<TextField>,
        suggestions: &[String],
        menu: Option<SearchSlot>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = menu == Some(slot) && !suggestions.is_empty();
        let id = match slot {
            SearchSlot::Title => "lyrics-suggest-title",
            SearchSlot::Artist => "lyrics-suggest-artist",
            SearchSlot::Album => "lyrics-suggest-album",
        };
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(56.))
                            .text_size(px(12.))
                            .text_color(theme::text_muted())
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(28.))
                            .px(px(6.))
                            .rounded_md()
                            .bg(theme::bg_deep())
                            .border_1()
                            .border_color(theme::border())
                            .child(field),
                    )
                    .child(self.button(
                        id,
                        "Suggestions".into(),
                        !suggestions.is_empty(),
                        open,
                        cx,
                        move |this, cx| {
                            this.search_menu = if this.search_menu == Some(slot) {
                                None
                            } else {
                                Some(slot)
                            };
                            cx.notify();
                        },
                    )),
            )
            .when(open, |col| {
                let suggestions = suggestions.to_vec();
                col.child(
                    div()
                        .ml(px(64.))
                        .flex()
                        .flex_col()
                        .rounded_md()
                        .bg(theme::bg_elevated())
                        .border_1()
                        .border_color(theme::border())
                        .children(suggestions.into_iter().enumerate().map(
                            |(index, suggestion)| {
                                let id = SharedString::from(format!("suggest-{label}-{index}"));
                                let value = suggestion.clone();
                                div()
                                    .id(id)
                                    .h(px(28.))
                                    .px(px(8.))
                                    .flex()
                                    .items_center()
                                    .text_size(px(12.))
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme::hover()))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |this, _: &gpui::MouseDownEvent, _, cx| {
                                                cx.stop_propagation();
                                                let value = value.clone();
                                                match slot {
                                                    SearchSlot::Title => {
                                                        this.search_title.update(cx, |field, cx| {
                                                            field.replace_text(&value, cx)
                                                        })
                                                    }
                                                    SearchSlot::Artist => this
                                                        .search_artist
                                                        .update(cx, |field, cx| {
                                                            field.replace_text(&value, cx)
                                                        }),
                                                    SearchSlot::Album => {
                                                        this.search_album.update(cx, |field, cx| {
                                                            field.replace_text(&value, cx)
                                                        })
                                                    }
                                                }
                                                this.search_menu = None;
                                                cx.notify();
                                            },
                                        ),
                                    )
                                    .child(suggestion)
                            },
                        )),
                )
            })
            .into_any_element()
    }

    fn suggestion_lists(&self) -> (Vec<String>, Vec<String>, Vec<String>) {
        let Some(snap) = self.primary() else {
            return (Vec::new(), Vec::new(), Vec::new());
        };
        let title = squash(&snap.title);
        let artist = squash(&snap.artist);
        let album = squash(&snap.album);
        let album_artist = squash(&self.row_value("AlbumArtist"));
        let performer = squash(&self.row_value("Performer"));
        let artist_parts = [artist.clone(), album_artist, performer]
            .into_iter()
            .flat_map(|value| {
                value
                    .split(',')
                    .map(|part| squash(part))
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let title_tail = if title.contains(" - ") {
            title.split(" - ").skip(1).collect::<Vec<_>>().join(" - ")
        } else {
            String::new()
        };
        (
            suggestions_for([
                title.clone(),
                strip_feat_tail(&title),
                strip_brackets(&title),
                title_tail,
            ]),
            suggestions_for(artist_parts.into_iter().chain(std::iter::once(artist))),
            suggestions_for([album.clone(), strip_brackets(&album)]),
        )
    }

    fn muzeeka(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let global = self.session.read(cx).rack.playback_rate;
        let draft = self.rate_draft;
        let frac = ((draft - 0.25) / 1.75).clamp(0.0, 1.0);
        let bpm_text = self.bpm_field.read(cx).content.to_string();
        let bpm_ok = parse_bpm(&bpm_text).is_some();
        let taps = self.tap_times.len();
        let video = self.video_path.clone();
        let snap = self.primary().cloned();
        let can_search = snap
            .as_ref()
            .is_some_and(|snap| !snap.title.trim().is_empty() && !snap.artist.trim().is_empty());
        let host = cx.entity().clone();
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(heading(
                "Muzeeka",
                "App extras for this track. Speed stays in Muzeeka only; BPM is written into the file tags.",
            ))
            .when(self.batch(), |col| {
                col.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child("Speed, BPM, lyrics, and video follow the first track."),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_size(px(13.)).child("Playback speed"))
                            .child(
                                div().text_size(px(12.)).text_color(theme::text_muted()).child(
                                    if self.rate_override.is_some() {
                                        "Custom for this track (app-only)".to_string()
                                    } else {
                                        format!("Using global speed from Settings ({global:.2}×)")
                                    },
                                ),
                            ),
                    )
                    .child(div().text_size(px(18.)).child(format!("{draft:.2}×"))),
            )
            .child(
                div()
                    .id("properties-rate")
                    .relative()
                    .h(px(28.))
                    .rounded_md()
                    .cursor_pointer()
                    .child(
                        canvas(
                            move |bounds, _window, app| {
                                host.update(app, |view, _cx| view.rate_bounds = Some(bounds));
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(12.))
                            .left(px(0.))
                            .right(px(0.))
                            .h(px(4.))
                            .rounded_full()
                            .bg(theme::bg_elevated())
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(frac))
                                    .rounded_full()
                                    .bg(theme::accent()),
                            ),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                            this.rate_drag = true;
                            this.rate_draft = this.rate_at(event.position.x);
                            cx.notify();
                        }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child("0.25×")
                    .child("2.00×"),
            )
            .child({
                let mut row = div().flex().flex_wrap().gap(px(8.));
                for rate in RATE_PRESETS {
                    let label = if (rate - 1.0).abs() < 0.001 {
                        "1.0×".to_string()
                    } else {
                        format!("{rate:.2}×")
                    };
                    let id = match (rate * 100.0).round() as i32 {
                        75 => "rate-75",
                        85 => "rate-85",
                        100 => "rate-100",
                        125 => "rate-125",
                        _ => "rate-150",
                    };
                    let active = self.rate_override.is_some_and(|current| (current - rate).abs() < 0.01);
                    row = row.child(self.button(id, label, !self.loading, active, cx, move |this, cx| {
                        this.rate_draft = rate;
                        this.commit_rate(Some(rate), cx);
                    }));
                }
                row.child(self.button(
                    "rate-reset",
                    "Reset".into(),
                    self.rate_override.is_some(),
                    false,
                    cx,
                    |this, cx| this.commit_rate(None, cx),
                ))
                .into_any_element()
            })
            .child(div().h(px(1.)).bg(theme::border()))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_size(px(13.)).child("BPM"))
                            .child(
                                div().text_size(px(12.)).text_color(theme::text_muted()).child(
                                    self.bpm_value
                                        .map(|bpm| format!("In file tags: {}", format_bpm(bpm)))
                                        .unwrap_or_else(|| "No BPM in tags yet — tap, detect, or type, then Write".into()),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(18.))
                            .child(parse_bpm(&bpm_text).map(format_bpm).unwrap_or_else(|| "—".into())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .w(px(88.))
                            .h(px(28.))
                            .px(px(8.))
                            .rounded_md()
                            .bg(theme::bg_deep())
                            .border_1()
                            .border_color(theme::border())
                            .child(self.bpm_field.clone()),
                    )
                    .child(self.button(
                        "bpm-tap",
                        if taps > 0 { format!("Tap · {taps}") } else { "Tap".into() },
                        !self.bpm_busy && !self.bpm_detecting,
                        false,
                        cx,
                        |this, cx| this.tap_bpm(cx),
                    ))
                    .child(self.button(
                        "bpm-detect",
                        if self.bpm_detecting { "Detecting…".into() } else { "Auto detect".into() },
                        !self.bpm_busy && !self.bpm_detecting,
                        false,
                        cx,
                        |this, cx| this.detect_bpm(cx),
                    ))
                    .child(self.button(
                        "bpm-write",
                        if self.bpm_busy { "Writing…".into() } else { "Write".into() },
                        bpm_ok && !self.bpm_busy && !self.bpm_detecting,
                        true,
                        cx,
                        |this, cx| this.write_bpm(cx),
                    )),
            )
            .child(div().h(px(1.)).bg(theme::border()))
            .child(div().text_size(px(13.)).child("Video background"))
            .child(
                div().text_size(px(12.)).text_color(theme::text_muted()).child(
                    if video.is_some() {
                        "Set — stored for this track"
                    } else {
                        "Not set"
                    },
                ),
            )
            .when_some(video.clone(), |col, path| {
                col.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_secondary())
                        .child(path),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .child(self.button(
                        "video-pick",
                        if video.is_some() { "Change…".into() } else { "Choose video…".into() },
                        !self.video_busy,
                        false,
                        cx,
                        |this, cx| this.pick_video(cx),
                    ))
                    .child(self.button(
                        "video-search",
                        if self.video_busy { "Searching…".into() } else { "Search YouTube".into() },
                        can_search && !self.video_busy,
                        false,
                        cx,
                        |this, cx| this.search_video(cx),
                    ))
                    .when(video.is_some(), |row| {
                        row.child(self.button(
                            "video-clear",
                            "Clear".into(),
                            !self.video_busy,
                            false,
                            cx,
                            |this, cx| this.clear_video(cx),
                        ))
                    }),
            )
            .child(self.status_bar(
                self.extra_error.clone(),
                self.extra_status.clone(),
                if self.video_path.is_some() {
                    "Video background active"
                } else if self.rate_override.is_some() {
                    "Speed override active"
                } else {
                    ""
                },
                cx,
                false,
            ))
            .into_any_element()
    }

    fn status_bar(
        &mut self,
        error: Option<String>,
        success: Option<String>,
        muted: &str,
        cx: &mut Context<Self>,
        save: bool,
    ) -> AnyElement {
        let text = error
            .clone()
            .or(success)
            .unwrap_or_else(|| muted.to_string());
        let danger = error.is_some();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .pt(px(8.))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(12.))
                    .text_color(if danger {
                        theme::danger()
                    } else {
                        theme::text_muted()
                    })
                    .child(text),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(self.button(
                        "properties-dismiss",
                        "Close".into(),
                        true,
                        false,
                        cx,
                        |this, cx| {
                            this.pending_close = true;
                            cx.notify();
                        },
                    ))
                    .when(save, |row| {
                        row.child(self.button(
                            "properties-save",
                            if self.saving {
                                "Saving…".into()
                            } else {
                                "Save".into()
                            },
                            !self.saving && !self.loading,
                            true,
                            cx,
                            |this, cx| this.save_tags(cx),
                        ))
                    }),
            )
            .into_any_element()
    }
}

impl Render for PropertiesWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let missing = self
            .primary()
            .is_some_and(|snap| self.session.read(cx).track(snap.id).is_none());
        if missing || self.pending_close {
            self.pending_close = false;
            window.defer(cx, |window, _cx| window.remove_window());
        }
        let title = if self.snaps.len() > 1 {
            format!("{} tracks | properties", self.snaps.len())
        } else if let Some(snap) = self.primary() {
            if snap.artist.is_empty() {
                format!("{} | properties", snap.title)
            } else if snap.title.is_empty() {
                format!("{} | properties", snap.artist)
            } else {
                format!("{} - {} | properties", snap.artist, snap.title)
            }
        } else {
            "properties".into()
        };
        if title != self.shown_title {
            self.shown_title = title.clone();
            window.set_window_title(&title);
        }
        let session = self.session.clone();
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::bg_deep())
            .text_color(theme::text())
            .when(self.use_ui_font, |root| {
                root.font_family(theme::FONT_FAMILY)
            })
            .key_context("Properties")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SaveProperties, _, cx| this.save_shortcut(cx)))
            .on_action(cx.listener(|this, _: &CloseProperties, window, cx| this.close(window, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, window, cx| {
                    if window.default_prevented() {
                        return;
                    }
                    this.focus.focus(window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.rate_drag {
                    this.drag_rate(event.position.x, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.end_rate_drag(cx);
                    this.armed = None;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.end_rate_drag(cx);
                    this.armed = None;
                }),
            )
            .child(crate::ui::main_window::alt_wheel_layer(session))
            .child(self.header(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .gap(px(8.))
                    .px(px(8.))
                    .pb(px(8.))
                    .child(self.section_list(cx))
                    .child(self.body(cx)),
            )
    }
}

fn text_field(
    cx: &mut Context<PropertiesWindow>,
    id: &str,
    placeholder: &str,
    host: gpui::Entity<PropertiesWindow>,
    multiline: bool,
    on_change: impl Fn(&str, &mut Window, &mut App) + 'static,
) -> gpui::Entity<TextField> {
    let id = SharedString::from(id.to_string());
    let placeholder = SharedString::from(placeholder.to_string());
    cx.new(move |cx| {
        let mut field = TextField::new(cx, placeholder, on_change, escape_cb(host)).element_id(id);
        if multiline {
            field = field.multiline();
        }
        field
    })
}

fn ignore_text(_: &str, _: &mut Window, _: &mut App) {}

fn watch_text(
    host: gpui::Entity<PropertiesWindow>,
) -> impl Fn(&str, &mut Window, &mut App) + 'static {
    move |_text, window, app| {
        let host = host.clone();
        window.defer(app, move |_window, app| {
            host.update(app, |_view, cx| cx.notify());
        });
    }
}

fn watch_lyrics(
    host: gpui::Entity<PropertiesWindow>,
) -> impl Fn(&str, &mut Window, &mut App) + 'static {
    move |text, window, app| {
        let host = host.clone();
        let text = text.to_string();
        window.defer(app, move |_window, app| {
            host.update(app, |view, cx| view.note_lyrics(&text, cx));
        });
    }
}

pub(crate) fn properties_key(paths: &[String]) -> String {
    let mut paths = paths.to_vec();
    paths.sort();
    paths.join("\n")
}

fn row_field(
    cx: &mut Context<PropertiesWindow>,
    row: &TagTableRow,
    mixed: bool,
    host: gpui::Entity<PropertiesWindow>,
) -> gpui::Entity<TextField> {
    let id = row.id.clone();
    let element = SharedString::from(format!("tag-{}", row.id));
    let placeholder = SharedString::from(if mixed { "<multiple values>" } else { "" });
    let multiline = is_multiline_field(&row.id);
    cx.new(move |cx| {
        let host_change = host.clone();
        let row_id = id.clone();
        let mut field = TextField::new(
            cx,
            placeholder,
            move |text, window, app| {
                let host = host_change.clone();
                let row_id = row_id.clone();
                let text = text.to_string();
                window.defer(app, move |_window, app| {
                    host.update(app, |view, cx| view.note_row(&row_id, &text, cx));
                });
            },
            escape_cb(host.clone()),
        )
        .element_id(element);
        if multiline {
            field = field.multiline();
        }
        field
    })
}

fn escape_cb(host: gpui::Entity<PropertiesWindow>) -> impl Fn(&mut Window, &mut App) + 'static {
    move |window, app| {
        let menu = host.update(app, |view, cx| {
            if view.search_menu.take().is_some() {
                cx.notify();
                true
            } else {
                false
            }
        });
        if !menu {
            window.remove_window();
        }
    }
}

fn heading(title: &'static str, detail: impl Into<SharedString>) -> AnyElement {
    let detail = detail.into();
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(18.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .child(detail),
        )
        .into_any_element()
}

fn is_multiline_field(id: &str) -> bool {
    matches!(id, "Comment" | "Description" | "Lyrics" | "UnsyncLyrics")
}

fn blank_to_none(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn duration_secs(duration: f64) -> Option<u32> {
    if duration.is_finite() && duration > 0.0 {
        Some(duration.round() as u32)
    } else {
        None
    }
}

fn cascade_bounds(key: &str, cx: &App) -> Bounds<Pixels> {
    let mut bounds = Bounds::centered(None, size(px(900.), px(700.)), cx);
    let mut hash: u32 = 2_166_136_261;
    for byte in key.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(1_677_7619);
    }
    let step = (hash % 10) as f32;
    bounds.origin.x += px(48. + step * 28.);
    bounds.origin.y += px(48. + step * 28.);
    bounds
}

fn tag_write_path(track_path: &str) -> Result<PathBuf, String> {
    if is_cue_track_path(track_path) {
        let (audio, _) = cue::parse_virtual_cue_path(track_path)
            .ok_or_else(|| format!("Could not resolve CUE audio file for: {track_path}"))?;
        let path = PathBuf::from(audio);
        if !path.is_file() {
            return Err(format!("CUE audio file not found: {}", path.display()));
        }
        return Ok(path);
    }
    let path = PathBuf::from(file_on_disk(track_path));
    if !path.is_file() {
        return Err(format!("File not found: {}", path.display()));
    }
    Ok(path)
}

fn mime_for(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("gif") => Some("image/gif"),
        Some("bmp") => Some("image/bmp"),
        Some("webp") => Some("image/webp"),
        _ => None,
    }
}

struct LoadResult {
    rows: Vec<TagTableRow>,
    mixed: HashSet<String>,
    tech: AudioTechInfo,
    lyrics: String,
    rate: Option<f32>,
    bpm_value: Option<f32>,
    bpm_text: String,
    video: Option<String>,
    error: Option<String>,
}

fn load_blocking(snaps: &[Snap]) -> LoadResult {
    let mut result = LoadResult {
        rows: Vec::new(),
        mixed: HashSet::new(),
        tech: AudioTechInfo::default(),
        lyrics: String::new(),
        rate: None,
        bpm_value: None,
        bpm_text: String::new(),
        video: None,
        error: None,
    };
    let Some(primary) = snaps.first() else {
        result.error = Some("Track was not found".into());
        return result;
    };
    let db = match catalog::open_library() {
        Ok(db) => db,
        Err(error) => {
            result.error = Some(error);
            return result;
        }
    };
    let mut tables = Vec::new();
    for snap in snaps {
        match read_rows(&db, snap) {
            Ok(rows) => tables.push(rows),
            Err(error) => {
                result.error = Some(error);
                tables.push(Vec::new());
            }
        }
    }
    if snaps.len() > 1 {
        let (rows, mixed) = merge_tables(&tables);
        result.rows = rows;
        result.mixed = mixed;
    } else {
        result.rows = tables.into_iter().next().unwrap_or_default();
    }
    if let Ok(path) = tag_write_path(&primary.path) {
        result.tech = tech::read_audio_tech_info(&path);
    }
    if let Ok(stats) = catalog::playback_stats(&db, &primary.path) {
        result.tech.play_count = Some(stats.play_count);
        result.tech.first_played_unix = stats.first_played_unix;
        result.tech.last_played_unix = stats.last_played_unix;
    }
    result.lyrics = crate::ui::lyrics_store::read_cached(
        &primary.title,
        &primary.artist,
        Some(primary.album.as_str()),
        duration_secs(primary.duration),
    )
    .unwrap_or_default();
    if result.lyrics.trim().is_empty() {
        if let Ok(path) = tag_write_path(&primary.path) {
            if let Ok(Some(embedded)) = tag_table::read_embedded_lyrics(&path) {
                result.lyrics = embedded;
            }
        }
    }
    result.rate = catalog::track_playback_rate(&db, &primary.path)
        .ok()
        .flatten();
    result.video = catalog::track_video_bg(&db, &primary.path).ok().flatten();
    let bpm = tag_write_path(&primary.path)
        .ok()
        .and_then(|path| tag_table::read_bpm(&path))
        .or_else(|| {
            parse_bpm(&row_in(&result.rows, "Bpm"))
                .or_else(|| parse_bpm(&row_in(&result.rows, "IntegerBpm")))
        });
    result.bpm_value = bpm;
    result.bpm_text = bpm.map(format_bpm).unwrap_or_default();
    result
}

fn read_rows(db: &rusqlite::Connection, snap: &Snap) -> Result<Vec<TagTableRow>, String> {
    if is_stream_url(&snap.path) {
        let fields = catalog::library_fields(db, snap.id.0 as i64).unwrap_or(LibraryFields {
            title: snap.title.clone(),
            artist: snap.artist.clone(),
            album: snap.album.clone(),
            ..LibraryFields::default()
        });
        return Ok(tag_table::tag_table_from_core_fields(
            Some(&fields.title),
            Some(&fields.artist),
            Some(&fields.album),
            fields.genre.as_deref(),
            fields.year,
            fields.track_number,
        ));
    }
    let path = tag_write_path(&snap.path)?;
    tag_table::read_tag_table(&path)
}

fn merge_tables(tables: &[Vec<TagTableRow>]) -> (Vec<TagTableRow>, HashSet<String>) {
    let mut order = Vec::new();
    let mut seen = HashSet::new();
    for table in tables {
        for row in table {
            if seen.insert(row.id.clone()) {
                order.push(row.id.clone());
            }
        }
    }
    let mut mixed = HashSet::new();
    let mut rows = Vec::new();
    for id in order {
        let Some(prototype) = tables
            .iter()
            .find_map(|table| table.iter().find(|row| row.id == id))
        else {
            continue;
        };
        let values = tables
            .iter()
            .map(|table| {
                table
                    .iter()
                    .find(|row| row.id == id)
                    .map(|row| row.value.clone())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let differs = values.iter().any(|value| value != &values[0]);
        if differs {
            mixed.insert(id);
        }
        let mut row = prototype.clone();
        row.value = if differs {
            String::new()
        } else {
            values.first().cloned().unwrap_or_default()
        };
        row.read_only = tables.iter().any(|table| {
            table
                .iter()
                .find(|row| row.id == prototype.id)
                .is_some_and(|row| row.read_only)
        });
        rows.push(row);
    }
    (rows, mixed)
}

fn row_in(rows: &[TagTableRow], id: &str) -> String {
    rows.iter()
        .find(|row| row.id == id)
        .map(|row| row.value.clone())
        .unwrap_or_default()
}

struct SavedTrack {
    id: TrackId,
    title: String,
    artist: String,
    album: String,
}

struct SaveOutcome {
    saved: Vec<SavedTrack>,
    rows: Vec<TagTableRow>,
    mixed: HashSet<String>,
    message: String,
    error: Option<String>,
}

fn save_tags_blocking(
    snaps: &[Snap],
    rows: &[TagTableRow],
    changed: &HashSet<String>,
    batch: bool,
) -> SaveOutcome {
    let mut outcome = SaveOutcome {
        saved: Vec::new(),
        rows: rows.to_vec(),
        mixed: HashSet::new(),
        message: String::new(),
        error: None,
    };
    let db = match catalog::open_library() {
        Ok(db) => db,
        Err(error) => {
            outcome.error = Some(error);
            return outcome;
        }
    };
    let mut failures = Vec::new();
    let mut wrote_file = 0usize;
    let mut wrote_cue = 0usize;
    let mut wrote_library = 0usize;
    for snap in snaps {
        let payload = if batch {
            match read_rows(&db, snap) {
                Ok(base) => apply_changed(&base, rows, changed),
                Err(error) => {
                    failures.push(format!("{}: {error}", file_name(&snap.path)));
                    continue;
                }
            }
        } else {
            rows.to_vec()
        };
        let file_rows = if batch {
            payload
                .iter()
                .filter(|row| changed.contains(&row.id) && !row.read_only)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            payload.clone()
        };
        match write_one(&db, snap, &payload, &file_rows) {
            Ok(kind) => {
                match kind {
                    WriteKind::Stream => wrote_library += 1,
                    WriteKind::Cue => wrote_cue += 1,
                    WriteKind::File => wrote_file += 1,
                }
                let fields = tag_table::track_fields_from_table(&payload);
                outcome.saved.push(SavedTrack {
                    id: snap.id,
                    title: fields.title.unwrap_or_default(),
                    artist: fields.artist.unwrap_or_default(),
                    album: fields.album.unwrap_or_default(),
                });
            }
            Err(error) => failures.push(format!("{}: {error}", file_name(&snap.path))),
        }
    }
    if failures.is_empty() {
        outcome.message = if snaps.len() > 1 {
            format!("Saved {} tracks", snaps.len())
        } else if wrote_library > 0 && wrote_file == 0 && wrote_cue == 0 {
            "Saved to library".into()
        } else if wrote_cue > 0 {
            "Tags written to audio image + library".into()
        } else {
            "Tags written to file + library".into()
        };
        if let Some(snap) = snaps.first() {
            if let Ok(fresh) = read_rows(&db, snap) {
                if snaps.len() == 1 {
                    outcome.rows = fresh;
                }
            }
        }
    } else {
        outcome.error = Some(failures.join("\n"));
    }
    outcome
}

enum WriteKind {
    Stream,
    Cue,
    File,
}

fn write_one(
    db: &rusqlite::Connection,
    snap: &Snap,
    library_rows: &[TagTableRow],
    file_rows: &[TagTableRow],
) -> Result<WriteKind, String> {
    let fields = tag_table::track_fields_from_table(library_rows);
    let title = fields.title.clone().unwrap_or_default();
    let artist = fields.artist.clone().unwrap_or_default();
    let album = fields.album.clone().unwrap_or_default();
    if is_stream_url(&snap.path) {
        catalog::update_track_core(
            db,
            snap.id.0 as i64,
            &title,
            &artist,
            &album,
            fields.genre.as_deref(),
            fields.year,
            fields.track_number,
            None,
        )?;
        return Ok(WriteKind::Stream);
    }
    let path = tag_write_path(&snap.path)?;
    if !file_rows.is_empty() {
        tag_table::write_tag_table(&path, file_rows)?;
    }
    catalog::update_track_core(
        db,
        snap.id.0 as i64,
        &title,
        &artist,
        &album,
        fields.genre.as_deref(),
        fields.year,
        fields.track_number,
        None,
    )?;
    if is_cue_track_path(&snap.path) {
        Ok(WriteKind::Cue)
    } else {
        Ok(WriteKind::File)
    }
}

fn apply_changed(
    base: &[TagTableRow],
    edited: &[TagTableRow],
    changed: &HashSet<String>,
) -> Vec<TagTableRow> {
    let mut rows = base.to_vec();
    for edit in edited {
        if edit.read_only || !changed.contains(&edit.id) {
            continue;
        }
        if let Some(row) = rows.iter_mut().find(|row| row.id == edit.id) {
            row.value = edit.value.clone();
        } else {
            rows.push(edit.clone());
        }
    }
    rows
}

fn write_cover_blocking(
    snap: &Snap,
    bytes: &[u8],
    mime: Option<&str>,
    siblings: &[TrackId],
) -> Result<(String, PathBuf, Vec<TrackId>), String> {
    let path = tag_write_path(&snap.path)?;
    muzeeka::audio::cover_write::write_track_cover(&path, bytes, mime)?;
    let (cover_id, thumb) = crate::ui::import::stored_cover(&path)
        .ok_or_else(|| "Cover was written but the thumbnail could not be built".to_string())?;
    let db = catalog::open_library()?;
    let ids = if siblings.is_empty() {
        vec![snap.id]
    } else {
        siblings.to_vec()
    };
    for id in &ids {
        catalog::set_cover_id(&db, id.0 as i64, &cover_id)?;
    }
    let message = if is_cue_track_path(&snap.path) {
        "Cover embedded into audio image"
    } else {
        "Cover embedded into file"
    };
    Ok((message.into(), thumb, ids))
}

fn file_name(path: &str) -> String {
    file_on_disk(path)
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(path)
        .to_string()
}

fn tech_summary(tech: &AudioTechInfo, duration: f64, path: &str) -> String {
    let mut parts = Vec::new();
    let seconds = tech.duration_secs.unwrap_or(duration);
    if seconds > 0.0 {
        parts.push(session::format_time(seconds).to_string());
    }
    if let Some(bitrate) = tech.bitrate_kbps {
        parts.push(format!("{bitrate} kbps"));
    }
    if let Some(rate) = tech.sample_rate_hz {
        parts.push(format!("{rate} Hz"));
    }
    if let Some(channels) = tech.channels {
        parts.push(format!("{channels} ch"));
    }
    if let Some(size) = tech.file_size {
        parts.push(format_bytes(size));
    }
    let ext = Path::new(file_on_disk(path))
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_uppercase());
    if let Some(ext) = ext {
        parts.push(ext);
    }
    if parts.is_empty() {
        "—".into()
    } else {
        parts.join(" · ")
    }
}

fn detail_groups(
    tech: &AudioTechInfo,
    snap: Option<&Snap>,
) -> Vec<(&'static str, Vec<(&'static str, String)>)> {
    let path = snap.map(|snap| snap.path.as_str()).unwrap_or("");
    let disk = tech
        .file_path
        .clone()
        .unwrap_or_else(|| file_on_disk(path).to_string());
    let folder = tech
        .folder_name
        .clone()
        .filter(|folder| !folder.is_empty())
        .unwrap_or_else(|| {
            let cleaned = file_on_disk(path);
            let index = cleaned.rfind(['\\', '/']).unwrap_or(0);
            if index == 0 {
                String::new()
            } else {
                cleaned[..index].to_string()
            }
        });
    vec![
        (
            "Location",
            vec![
                (
                    "File name",
                    dash(tech.file_name.as_deref().unwrap_or(&file_name(path))),
                ),
                ("Folder name", dash(&folder)),
                ("File path", dash(&disk)),
                ("Subsong index", subsong_index(path).to_string()),
                (
                    "File size",
                    tech.file_size
                        .map(format_bytes)
                        .unwrap_or_else(|| "—".into()),
                ),
                ("Modified", format_unix(tech.modified_unix)),
                ("Created", format_unix(tech.created_unix)),
            ],
        ),
        (
            "General",
            vec![
                (
                    "Duration",
                    tech.duration_secs
                        .or(snap.map(|snap| snap.duration))
                        .filter(|seconds| *seconds > 0.0)
                        .map(|seconds| session::format_time(seconds).to_string())
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    "Sample rate",
                    tech.sample_rate_hz
                        .map(|rate| format!("{rate} Hz"))
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    "Channels",
                    tech.channels
                        .map(|channels| channels.to_string())
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    "Bit depth",
                    tech.bit_depth
                        .filter(|bits| *bits > 0)
                        .map(|bits| bits.to_string())
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    "Bitrate",
                    tech.bitrate_kbps
                        .map(|rate| format!("{rate} kbps"))
                        .unwrap_or_else(|| "—".into()),
                ),
                ("Codec", dash(tech.codec.as_deref().unwrap_or(""))),
                ("Encoding", dash(tech.encoding.as_deref().unwrap_or(""))),
                ("Tool", dash(tech.tool.as_deref().unwrap_or(""))),
                ("Embedded cuesheet", yes_no(tech.embedded_cuesheet)),
                ("Audio MD5", dash(tech.audio_md5.as_deref().unwrap_or(""))),
            ],
        ),
        (
            "Playback Statistics",
            vec![
                (
                    "Play count",
                    format_play_count(tech.play_count.unwrap_or(0)),
                ),
                ("First played", format_unix(tech.first_played_unix)),
                ("Last played", format_unix(tech.last_played_unix)),
            ],
        ),
    ]
}

fn dash(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "—".into()
    } else {
        value.to_string()
    }
}

fn yes_no(value: Option<bool>) -> String {
    match value {
        Some(true) => "yes".into(),
        Some(false) => "no".into(),
        None => "—".into(),
    }
}

fn format_play_count(count: u32) -> String {
    if count == 1 {
        "1 time".into()
    } else {
        format!("{count} times")
    }
}

fn format_bytes(size: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn format_unix(unix: Option<i64>) -> String {
    let Some(unix) = unix.filter(|unix| *unix > 0) else {
        return "—".into();
    };
    format_local(unix).unwrap_or_else(|| format_utc(unix))
}

fn format_utc(unix: i64) -> String {
    let day = unix.div_euclid(86_400);
    let time = unix.rem_euclid(86_400);
    let z = day + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let hour = time / 3600;
    let minute = (time % 3600) / 60;
    let second = time % 60;
    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}:{second:02} UTC")
}

fn format_local(unix: i64) -> Option<String> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ticks = (unix as u64).saturating_add(11_644_473_600) * 10_000_000;
    let file = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    unsafe { FileTimeToSystemTime(&file, &mut utc) }.ok()?;
    let mut local = SYSTEMTIME::default();
    unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.ok()?;
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
    ))
}

fn subsong_index(path: &str) -> u32 {
    path.split_once("#cue:")
        .and_then(|(_, rest)| rest.parse::<u32>().ok())
        .filter(|index| *index > 0)
        .map(|index| index - 1)
        .unwrap_or(0)
}

fn format_bpm(bpm: f32) -> String {
    if (bpm - bpm.round()).abs() < 0.05 {
        format!("{}", bpm.round() as u32)
    } else {
        format!("{bpm:.1}")
    }
}

fn parse_bpm(raw: &str) -> Option<f32> {
    let text = raw.trim().replace(',', ".");
    if text.is_empty() {
        return None;
    }
    let value: f32 = text.parse().ok()?;
    if !value.is_finite() || value < 1.0 || value >= 1000.0 {
        return None;
    }
    Some((value * 10.0).round() / 10.0)
}

fn median_bpm(times: &[Instant]) -> Option<f32> {
    if times.len() < 2 {
        return None;
    }
    let mut intervals = Vec::new();
    for pair in times.windows(2) {
        let ms = pair[1].saturating_duration_since(pair[0]).as_secs_f64() * 1000.0;
        if (200.0..=2000.0).contains(&ms) {
            intervals.push(ms);
        }
    }
    if intervals.is_empty() {
        return None;
    }
    intervals.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let mid = intervals[intervals.len() / 2];
    Some(((60_000.0 / mid) as f32 * 10.0).round() / 10.0)
}

fn squash(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn suggestions_for(candidates: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for candidate in candidates {
        let value = squash(&candidate);
        if value.is_empty() {
            continue;
        }
        let key = value.to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        out.push(value);
        if out.len() >= 6 {
            break;
        }
    }
    out
}

fn strip_brackets(value: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for ch in value.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    squash(&out)
}

fn strip_feat_tail(value: &str) -> String {
    let lower = value.to_lowercase();
    let mut cut = None;
    for marker in ["featuring", "feat.", "feat", "ft.", "ft", "with"] {
        let Some(index) = lower.rfind(marker) else {
            continue;
        };
        let before_ok = index == 0
            || lower[..index]
                .chars()
                .next_back()
                .is_some_and(|ch| ch.is_whitespace() || ch == '(' || ch == '[');
        if !before_ok {
            continue;
        }
        let after = lower[index + marker.len()..].trim_start_matches(['.', ' ']);
        if after.is_empty() {
            continue;
        }
        cut = Some(cut.map_or(index, |prev: usize| prev.min(index)));
    }
    match cut {
        Some(index) => {
            squash(value[..index].trim_end_matches(|ch: char| matches!(ch, '(' | '[' | ' ')))
        }
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_bpm, strip_brackets, strip_feat_tail, subsong_index};

    #[test]
    fn bpm_accepts_a_comma_and_rejects_zero() {
        assert_eq!(parse_bpm("128,5"), Some(128.5));
        assert_eq!(parse_bpm("98"), Some(98.0));
        assert!(parse_bpm("0").is_none());
        assert!(parse_bpm("1000").is_none());
    }

    #[test]
    fn feat_and_brackets_strip_to_the_base_title() {
        assert_eq!(strip_feat_tail("Song (feat. Guest)"), "Song");
        assert_eq!(strip_brackets("Album (Deluxe Edition)"), "Album");
        assert_eq!(subsong_index(r"D:\music\album.flac#cue:3"), 2);
        assert_eq!(subsong_index(r"D:\music\song.mp3"), 0);
    }
}
