//! Explorer drops on the main window.
//!
//! The OS owns the drag, so Ctrl is read with GetAsyncKeyState. Zone hit
//! rectangles come from the previous frame: canvas prepaint records them
//! after the view has already rendered.

use std::time::{Duration, Instant};

use gpui::{
    canvas, div, point, prelude::*, px, size, AnyElement, Bounds, Context, ExternalPaths,
    IntoElement, Pixels, Point, SpringState, Window,
};

use crate::ui::import::{self, ImportKind};
use crate::ui::main_window::MainWindow;
use crate::ui::session::LibraryView;
use crate::ui::theme;

#[cfg(windows)]
#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(vkey: i32) -> i16;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DropZone {
    None,
    Sidebar,
    Tracks,
}

pub(crate) struct FileDropState {
    active: bool,
    ctrl: bool,
    position: Point<Pixels>,
    zone: DropZone,
    sidebar_bounds: Bounds<Pixels>,
    track_bounds: Bounds<Pixels>,
    list_bounds: Bounds<Pixels>,
    rows: Vec<(String, String, Bounds<Pixels>)>,
    toast: Option<String>,
    toast_at: Instant,
    toast_hold: bool,
    last_key: Option<String>,
    last_at: Option<Instant>,
    scanning: bool,
    /// Every dragged path is a file. Folders keep the create-playlist hint.
    /// Set once, when the drag enters: the paths do not change after that.
    files_only: bool,
    /// Rows in the open list, from the last track-list render.
    /// A drag move must not rebuild that list.
    track_count: usize,
    /// Row boundary in the track list where a file drop will land.
    track_slot: Option<usize>,
    /// The open gap is a track reorder, not an Explorer file slot.
    reorder_rows: bool,
    /// Visual gap. The list reads this while the spring is still moving.
    gap: GapMotion,
    gap_clock: bool,
    gap_at: Instant,
}

impl Default for FileDropState {
    fn default() -> Self {
        Self {
            active: false,
            ctrl: false,
            position: point(px(0.), px(0.)),
            zone: DropZone::None,
            sidebar_bounds: zero_bounds(),
            track_bounds: zero_bounds(),
            list_bounds: zero_bounds(),
            rows: Vec::new(),
            toast: None,
            toast_at: Instant::now(),
            toast_hold: false,
            last_key: None,
            last_at: None,
            scanning: false,
            files_only: false,
            track_count: 0,
            track_slot: None,
            reorder_rows: false,
            gap: GapMotion::default(),
            gap_clock: false,
            gap_at: Instant::now(),
        }
    }
}

/// One spring shared by the dashed strip and every shifted row.
struct GapMotion {
    index: f32,
    index_v: f32,
    open: f32,
    open_v: f32,
    engaged: bool,
    /// A dropped file already occupies the hole, so the gap must not ease shut over it.
    snap: bool,
}

impl Default for GapMotion {
    fn default() -> Self {
        Self {
            index: 0.,
            index_v: 0.,
            open: 0.,
            open_v: 0.,
            engaged: false,
            snap: false,
        }
    }
}

fn spring_state(position: f32, velocity: f32) -> SpringState {
    SpringState { position, velocity }
}

/// Advances the gap. Returns whether another frame is needed.
fn step_gap(motion: &mut GapMotion, target: Option<f32>, dt: f32, reduce: bool) -> bool {
    let config = MainWindow::smooth_config();
    let dt = if dt.is_finite() { dt.max(0.) } else { 0. };
    const EPS: f32 = 0.012;
    match target {
        Some(goal) => {
            motion.snap = false;
            if !motion.engaged {
                motion.index = goal;
                motion.index_v = 0.;
                motion.open = 0.;
                motion.open_v = 0.;
                motion.engaged = true;
            }
            if reduce {
                motion.index = goal;
                motion.index_v = 0.;
                motion.open = 1.;
                motion.open_v = 0.;
                return false;
            }
            let index = config.step(spring_state(motion.index, motion.index_v), goal, dt);
            let open = config.step(spring_state(motion.open, motion.open_v), 1., dt);
            motion.index = index.position;
            motion.index_v = index.velocity;
            motion.open = open.position;
            motion.open_v = open.velocity;
            let index_done = config.is_settled(index, goal, EPS);
            let open_done = config.is_settled(open, 1., EPS);
            if index_done {
                motion.index = goal;
                motion.index_v = 0.;
            }
            if open_done {
                motion.open = 1.;
                motion.open_v = 0.;
            }
            !(index_done && open_done)
        }
        None => {
            if !motion.engaged {
                motion.snap = false;
                return false;
            }
            if motion.snap || reduce {
                *motion = GapMotion::default();
                return false;
            }
            let open = config.step(spring_state(motion.open, motion.open_v), 0., dt);
            motion.open = open.position;
            motion.open_v = open.velocity;
            if config.is_settled(open, 0., EPS) || motion.open <= 0. {
                *motion = GapMotion::default();
                false
            } else {
                true
            }
        }
    }
}

/// Track row height, header, and the list's top padding. The slot math has to
/// match `track_list`.
const TRACK_ROW_H: f32 = 52.;
const TRACK_HEADER_H: f32 = 36.;
const TRACK_LIST_PAD: f32 = 8.;

/// Index of the gap for a pointer `y` pixels into the list body. `count` is the
/// number of tracks. The result is `0..=count`.
fn insertion_index(y: f32, count: usize) -> usize {
    if count == 0 || y < TRACK_ROW_H * 0.5 {
        return 0;
    }
    let row = (y / TRACK_ROW_H).floor();
    let frac = y / TRACK_ROW_H - row;
    let index = if frac > 0.5 {
        row as usize + 1
    } else {
        row as usize
    };
    index.min(count)
}

fn zero_bounds() -> Bounds<Pixels> {
    Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.)))
}

fn ctrl_down() -> bool {
    #[cfg(windows)]
    {
        // High bit means the key is down. The drag does not deliver modifiers.
        unsafe {
            GetAsyncKeyState(0x11) < 0 || GetAsyncKeyState(0xA2) < 0 || GetAsyncKeyState(0xA3) < 0
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

impl FileDropState {
    fn zone_at(&self, position: Point<Pixels>) -> DropZone {
        if self.sidebar_bounds.size.width > px(1.) && self.sidebar_bounds.contains(&position) {
            DropZone::Sidebar
        } else if self.track_bounds.size.width > px(1.) && self.track_bounds.contains(&position) {
            DropZone::Tracks
        } else {
            DropZone::None
        }
    }

    fn row_at(&self) -> Option<(String, String)> {
        if self.list_bounds.size.height <= px(1.) || !self.list_bounds.contains(&self.position) {
            return None;
        }
        self.rows.iter().find_map(|(id, name, bounds)| {
            bounds
                .contains(&self.position)
                .then(|| (id.clone(), name.clone()))
        })
    }

    fn import_mode(&self) -> bool {
        self.ctrl || self.files_only
    }

    fn lit_row_id(&self) -> Option<String> {
        self.lit_row_id_at(self.active, self.import_mode(), self.zone, self.position)
            .map(str::to_owned)
    }

    /// Playlist row under the pointer, without cloning, while a file drag is in import mode.
    fn lit_row_id_at(
        &self,
        active: bool,
        import_mode: bool,
        zone: DropZone,
        position: Point<Pixels>,
    ) -> Option<&str> {
        if !(active && import_mode && zone == DropZone::Sidebar) {
            return None;
        }
        if self.list_bounds.size.height <= px(1.) || !self.list_bounds.contains(&position) {
            return None;
        }
        self.rows
            .iter()
            .find_map(|(id, _, bounds)| bounds.contains(&position).then_some(id.as_str()))
    }

    fn sidebar_lit(&self) -> bool {
        self.active && self.zone == DropZone::Sidebar && !self.import_mode()
    }

    fn tracks_lit(&self) -> bool {
        self.active && self.zone == DropZone::Tracks && !self.files_only
    }
}

impl MainWindow {
    pub(crate) fn note_file_drag(
        &mut self,
        event: &gpui::DragMoveEvent<ExternalPaths>,
        cx: &mut Context<Self>,
    ) {
        if crate::ui::file_drag_out::is_outgoing() {
            return;
        }
        let position = event.event.position;
        // Paths stay the ones captured when the drag entered the window.
        if !self.file_drop.active {
            let files_only = {
                let paths = event.drag(cx).paths();
                !paths.is_empty() && paths.iter().all(|path| path.is_file())
            };
            self.file_drop.files_only = files_only;
        }
        let ctrl = ctrl_down();
        let zone = self.file_drop.zone_at(position);
        let previous_slot = self.file_drop.track_slot;
        let was_active = self.file_drop.active;
        let ctrl_changed = self.file_drop.ctrl != ctrl;
        let zone_changed = self.file_drop.zone != zone;
        let row_changed = {
            let drop = &self.file_drop;
            let next_import = ctrl || drop.files_only;
            drop.lit_row_id_at(drop.active, drop.import_mode(), drop.zone, drop.position)
                != drop.lit_row_id_at(true, next_import, zone, position)
        };
        self.file_drop.active = true;
        self.file_drop.ctrl = ctrl;
        self.file_drop.zone = zone;
        self.file_drop.position = position;
        self.file_drop.reorder_rows = false;
        self.file_drop.track_slot = self.track_slot_at(position, cx);
        if !was_active
            || ctrl_changed
            || zone_changed
            || row_changed
            || previous_slot != self.file_drop.track_slot
        {
            cx.notify();
        }
    }

    pub(crate) fn set_drop_track_count(&mut self, count: usize) {
        self.file_drop.track_count = count;
    }

    fn track_slot_at(&self, position: Point<Pixels>, cx: &Context<Self>) -> Option<usize> {
        if !self.file_drop.files_only || self.file_drop.zone != DropZone::Tracks {
            return None;
        }
        let session = self.session.read(cx);
        if !matches!(session.view, LibraryView::All | LibraryView::Playlist(_)) {
            return None;
        }
        let count = self.file_drop.track_count;
        let scroll = -self.track_scroll.0.borrow().base_handle.offset().y;
        let top = self.file_drop.track_bounds.top();
        let y =
            (position.y - top - px(TRACK_HEADER_H) - px(TRACK_LIST_PAD) + scroll).to_f64() as f32;
        Some(insertion_index(y, count))
    }

    pub(crate) fn clear_file_drag(&mut self, cx: &mut Context<Self>) {
        if !self.file_drop.active {
            return;
        }
        self.file_drop.active = false;
        self.file_drop.ctrl = false;
        self.file_drop.zone = DropZone::None;
        self.file_drop.files_only = false;
        self.file_drop.track_slot = None;
        self.file_drop.reorder_rows = false;
        cx.notify();
    }

    pub(crate) fn receive_file_drop(
        &mut self,
        paths: &ExternalPaths,
        ignore: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if crate::ui::file_drag_out::is_outgoing() {
            return;
        }
        let listed: Vec<String> = paths
            .paths()
            .iter()
            .map(|path| path.to_string_lossy().trim().to_string())
            .filter(|path| !path.is_empty())
            .collect();
        self.file_drop.position = position;
        self.file_drop.ctrl = ctrl_down();
        self.file_drop.zone = if ignore {
            DropZone::None
        } else {
            self.file_drop.zone_at(position)
        };
        let zone = self.file_drop.zone;
        let ctrl = self.file_drop.ctrl;
        let files_only = !listed.is_empty()
            && listed
                .iter()
                .all(|path| std::path::Path::new(path).is_file());
        let import_mode = ctrl || files_only;
        let target = self.file_drop.row_at();
        let slot = self.file_drop.track_slot;
        // The new row is inserted into this hole. Easing the gap shut would stack on top of it.
        if files_only && zone == DropZone::Tracks && slot.is_some() {
            self.file_drop.gap.snap = true;
        }
        self.file_drop.active = false;
        self.file_drop.zone = DropZone::None;
        self.file_drop.ctrl = false;
        self.file_drop.files_only = false;
        self.file_drop.track_slot = None;

        if ignore || zone == DropZone::None {
            cx.notify();
            return;
        }
        if listed.is_empty() {
            self.show_drop_toast("Drop failed: no file paths received", false);
            cx.notify();
            return;
        }
        if zone == DropZone::Sidebar && import_mode && target.is_none() {
            let text = if files_only {
                "Drop on a playlist to import"
            } else {
                "Hold Ctrl and drop on a playlist to import"
            };
            self.show_drop_toast(text, false);
            cx.notify();
            return;
        }
        if self.file_drop.scanning {
            self.show_drop_toast("Scanning…", true);
            cx.notify();
            return;
        }
        let key = drop_key(&listed);
        if self.file_drop.last_key.as_deref() == Some(key.as_str())
            && self
                .file_drop
                .last_at
                .is_some_and(|at| at.elapsed() < Duration::from_secs(4))
        {
            cx.notify();
            return;
        }

        let sources = import::classify_drops(&listed);
        let open = {
            let session = self.session.read(cx);
            match &session.view {
                LibraryView::Playlist(id) => session
                    .playlists
                    .iter()
                    .find(|playlist| &playlist.id == id)
                    .map(|playlist| (playlist.id.clone(), playlist.name.clone())),
                LibraryView::All | LibraryView::Liked => None,
            }
        };
        let place_at =
            (files_only && zone == DropZone::Tracks && self.session.read(cx).sort.is_none())
                .then_some(slot)
                .flatten();
        let (kind, stage_playlist) = if zone == DropZone::Sidebar && import_mode {
            let (id, name) = target.expect("row checked above");
            (
                ImportKind::Playlist {
                    id: id.clone(),
                    label: format!("«{name}»"),
                    sources,
                    at: None,
                },
                Some(id),
            )
        } else if zone == DropZone::Sidebar {
            (ImportKind::Folders { sources }, None)
        } else if let Some((id, name)) = open {
            (
                ImportKind::Playlist {
                    id: id.clone(),
                    label: name,
                    sources,
                    at: place_at,
                },
                Some(id),
            )
        } else if files_only {
            (
                ImportKind::Library {
                    sources,
                    at: place_at,
                },
                None,
            )
        } else {
            (ImportKind::NewPlaylist { sources }, None)
        };

        if files_only && self.session.read(cx).has_library() {
            let staged = listed.clone();
            self.session.update(cx, |session, _cx| {
                session.stage_dropped_files(&staged, stage_playlist.as_deref(), place_at);
            });
            self.reveal_dropped(&listed, cx);
        }

        self.file_drop.last_key = Some(key);
        self.file_drop.last_at = Some(Instant::now());
        self.file_drop.scanning = true;
        self.show_drop_toast("Scanning…", true);
        cx.notify();
        let paths = listed;
        cx.spawn(async move |this, cx| {
            let files = cx
                .background_executor()
                .spawn(async move { import::scan_paths(&paths) })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.file_drop.scanning = false;
                view.search_hits = None;
                let result = view
                    .session
                    .update(cx, |session, _cx| session.import_dropped(&files, &kind));
                let reveal: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
                match result {
                    Ok(report) => view.show_drop_toast(report.toast, false),
                    Err(error) => {
                        view.show_drop_toast(format!("Drop failed: {error}"), false);
                        let _ = view
                            .session
                            .update(cx, |session, _cx| session.reload_lists());
                    }
                }
                view.reveal_dropped(&reveal, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn reveal_dropped(&mut self, paths: &[String], cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let keys: Vec<String> = paths
            .iter()
            .map(|path| crate::ui::paths::path_key(path))
            .collect();
        let row = {
            let session = self.session.read(cx);
            session.visible_indexes("").iter().position(|index| {
                session.track_by_index(*index).is_some_and(|track| {
                    let key = crate::ui::paths::path_key(&track.path);
                    keys.iter().any(|candidate| candidate == &key)
                })
            })
        };
        if let Some(row) = row {
            self.track_scroll
                .scroll_to_item(row, gpui::ScrollStrategy::Center);
        }
    }

    pub(crate) fn tick_file_drop(&mut self) -> bool {
        let mut dirty = false;
        if self.file_drop.active {
            let ctrl = ctrl_down();
            if ctrl != self.file_drop.ctrl {
                self.file_drop.ctrl = ctrl;
                dirty = true;
            }
        }
        if self.file_drop.toast.is_some()
            && !self.file_drop.toast_hold
            && self.file_drop.toast_at.elapsed() >= Duration::from_millis(3200)
        {
            self.file_drop.toast = None;
            dirty = true;
        }
        dirty
    }

    pub(crate) fn show_drop_toast(&mut self, text: impl Into<String>, hold: bool) {
        let text = text.into();
        if text.is_empty() {
            self.file_drop.toast = None;
            self.file_drop.toast_hold = false;
            return;
        }
        self.file_drop.toast = Some(text);
        self.file_drop.toast_hold = hold;
        self.file_drop.toast_at = Instant::now();
    }

    pub(crate) fn drop_row_lit(&self, id: &str) -> bool {
        self.file_drop.lit_row_id().as_deref() == Some(id)
            || self
                .item_drag
                .as_ref()
                .is_some_and(|drag| drag.targets_playlist(id))
    }

    pub(crate) fn sidebar_drop_lit(&self) -> bool {
        self.file_drop.sidebar_lit()
    }

    /// Steps the track-list gap. The logical slot stays on the cursor; this only eases the drawing.
    pub(crate) fn tick_drop_motion(&mut self, window: &mut Window, reduce_motion: bool) {
        let target = self.file_drop.track_slot.map(|slot| slot as f32);
        let now = Instant::now();
        let starting = target.is_some() && !self.file_drop.gap.engaged;
        let dt = if starting || !self.file_drop.gap_clock {
            1. / 60.
        } else {
            now.duration_since(self.file_drop.gap_at)
                .as_secs_f32()
                .clamp(0., 0.05)
        };
        self.file_drop.gap_clock = true;
        self.file_drop.gap_at = now;
        if step_gap(&mut self.file_drop.gap, target, dt, reduce_motion) {
            window.request_animation_frame();
        } else if self.file_drop.track_slot.is_none() {
            self.file_drop.reorder_rows = false;
        }
    }

    /// How tall the open gap is. A file slot is one row. A reorder gap is shorter.
    pub(crate) fn gap_span(&self) -> f32 {
        if self.file_drop.reorder_rows {
            crate::ui::item_drag::REORDER_GAP
        } else {
            TRACK_ROW_H
        }
    }

    pub(crate) fn reorder_gap(&self) -> bool {
        self.file_drop.reorder_rows
    }

    pub(crate) fn set_reorder_gap(&mut self, slot: Option<usize>) {
        self.file_drop.track_slot = slot;
        if slot.is_some() {
            self.file_drop.reorder_rows = true;
        }
    }

    pub(crate) fn drop_track_count(&self) -> usize {
        self.file_drop.track_count
    }

    pub(crate) fn track_panel_bounds(&self) -> Bounds<Pixels> {
        self.file_drop.track_bounds
    }

    pub(crate) fn playlist_list_bounds(&self) -> Bounds<Pixels> {
        self.file_drop.list_bounds
    }

    pub(crate) fn playlist_id_at(&self, position: Point<Pixels>) -> Option<String> {
        if self.file_drop.list_bounds.size.height <= px(1.)
            || !self.file_drop.list_bounds.contains(&position)
        {
            return None;
        }
        self.file_drop
            .rows
            .iter()
            .find_map(|(id, _, bounds)| bounds.contains(&position).then(|| id.clone()))
    }

    /// Row centers in sidebar content space, top to bottom.
    pub(crate) fn playlist_slot_centers(&self) -> Vec<f32> {
        let top = self.file_drop.list_bounds.top().as_f32();
        let scroll = -self.sidebar_scroll.offset().y.as_f32();
        let mut centers: Vec<f32> = self
            .file_drop
            .rows
            .iter()
            .map(|(_, _, bounds)| {
                bounds.top().as_f32() - top + scroll + bounds.size.height.as_f32() * 0.5
            })
            .collect();
        centers.sort_by(|left, right| left.total_cmp(right));
        centers
    }

    /// Visual insertion index and how far the gap has opened, while it is on screen.
    pub(crate) fn drop_gap(&self) -> Option<(f32, f32)> {
        if !self.file_drop.gap.engaged {
            None
        } else {
            Some((
                self.file_drop.gap.index,
                self.file_drop.gap.open.clamp(0., 1.),
            ))
        }
    }

    pub(crate) fn sidebar_drop_canvas(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        canvas(
            move |bounds, _window, app| {
                entity.update(app, |this, _cx| {
                    this.file_drop.rows.clear();
                    this.file_drop.list_bounds = zero_bounds();
                    this.file_drop.track_bounds = zero_bounds();
                    this.file_drop.sidebar_bounds = bounds;
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.))
    }

    pub(crate) fn playlist_list_canvas(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        canvas(
            move |bounds, _window, app| {
                entity.update(app, |this, _cx| {
                    this.file_drop.list_bounds = bounds;
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.))
    }

    pub(crate) fn playlist_drop_canvas(
        &self,
        id: String,
        name: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity().clone();
        canvas(
            move |bounds, _window, app| {
                entity.update(app, |this, _cx| {
                    this.file_drop.rows.push((id, name, bounds));
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.))
    }

    pub(crate) fn track_drop_canvas(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        canvas(
            move |bounds, _window, app| {
                let mut tint = false;
                entity.update(app, |this, _cx| {
                    this.file_drop.track_bounds = bounds;
                    tint = this.file_drop.tracks_lit();
                });
                tint
            },
            |bounds, tint, window, _app| {
                if tint {
                    window.paint_quad(gpui::fill(bounds, theme::accent_soft()));
                }
            },
        )
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.))
    }

    pub(crate) fn drop_overlay(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let hint = self.drop_hint(cx);
        let toast = self.file_drop.toast.clone();
        if hint.is_none() && toast.is_none() {
            return div().into_any_element();
        }
        let zone_bounds = match self.file_drop.zone {
            DropZone::Sidebar => self.file_drop.sidebar_bounds,
            DropZone::Tracks => self.file_drop.track_bounds,
            DropZone::None => zero_bounds(),
        };
        div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size(px(0.))
            .children(hint.map(|(title, detail)| {
                div()
                    .absolute()
                    .left(zone_bounds.left())
                    .top(zone_bounds.top())
                    .w(zone_bounds.size.width)
                    .h(zone_bounds.size.height)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(drop_card(title, detail))
            }))
            .children(toast.map(|text| drop_toast(&text, window)))
            .into_any_element()
    }

    fn drop_hint(&self, cx: &Context<Self>) -> Option<(String, String)> {
        if !self.file_drop.active {
            return None;
        }
        if self.file_drop.zone == DropZone::Sidebar && !self.file_drop.import_mode() {
            return Some((
                "Create playlist from folder".into(),
                "Release to create · hold Ctrl to import into a playlist".into(),
            ));
        }
        if self.file_drop.zone == DropZone::Sidebar && self.file_drop.files_only {
            if self.file_drop.lit_row_id().is_some() {
                return None;
            }
            return Some((
                "Add to a playlist".into(),
                "Drop the track on a playlist".into(),
            ));
        }
        if self.file_drop.zone == DropZone::Tracks && self.file_drop.files_only {
            return None;
        }
        if self.file_drop.zone == DropZone::Tracks {
            if let Some(name) = self.open_playlist_name(cx) {
                let detail = if self.file_drop.files_only {
                    "The track shows up in this list"
                } else {
                    "Drop files or folders here"
                };
                return Some((format!("Add to «{name}»"), detail.into()));
            }
            if self.file_drop.files_only {
                return Some((
                    "Add to the library".into(),
                    "The track shows up in this list".into(),
                ));
            }
            return Some((
                "Drop to create playlist".into(),
                "Drop files or folders here".into(),
            ));
        }
        None
    }

    fn open_playlist_name(&self, cx: &Context<Self>) -> Option<String> {
        let session = self.session.read(cx);
        match &session.view {
            LibraryView::Playlist(id) => session
                .playlists
                .iter()
                .find(|playlist| &playlist.id == id)
                .map(|playlist| playlist.name.clone()),
            LibraryView::All | LibraryView::Liked => None,
        }
    }
}

fn drop_card(title: String, detail: String) -> AnyElement {
    div()
        .max_w(px(280.))
        .px(px(18.))
        .py(px(14.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(4.))
        .rounded_md()
        .bg(gpui::rgba(0x111118f5))
        .border_1()
        .border_dashed()
        .border_color(theme::accent())
        .shadow_md()
        .child(
            div()
                .text_size(px(15.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .text_center()
                .child(title),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .text_center()
                .child(detail),
        )
        .into_any_element()
}

fn drop_toast(text: &str, window: &Window) -> AnyElement {
    let width = window.viewport_size().width.as_f32();
    let estimate = (text.chars().count() as f32 * 7.0 + 36.).clamp(80., 520.);
    let left = ((width - estimate) * 0.5).max(8.);
    div()
        .absolute()
        .bottom(px(118.))
        .left(px(left))
        .max_w(px(estimate))
        .px(px(14.))
        .py(px(8.))
        .rounded_md()
        .bg(theme::bg_elevated())
        .border_1()
        .border_color(theme::border())
        .shadow_md()
        .text_size(px(12.))
        .text_color(theme::text())
        .text_center()
        .child(text.to_string())
        .into_any_element()
}

fn drop_key(paths: &[String]) -> String {
    let mut keys: Vec<String> = paths.iter().map(|path| path.to_lowercase()).collect();
    keys.sort();
    keys.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{insertion_index, step_gap, GapMotion};

    #[test]
    fn slot_opens_on_the_nearest_row_boundary() {
        assert_eq!(insertion_index(-4.0, 5), 0);
        assert_eq!(insertion_index(10.0, 5), 0);
        assert_eq!(insertion_index(40.0, 5), 1);
        assert_eq!(insertion_index(52.0, 5), 1);
        assert_eq!(insertion_index(200.0, 2), 2);
        assert_eq!(insertion_index(80.0, 0), 0);
    }

    #[test]
    fn gap_opens_where_the_cursor_is_then_slides() {
        let mut motion = GapMotion::default();
        assert!(step_gap(&mut motion, Some(2.0), 1.0 / 60.0, false));
        assert!((motion.index - 2.0).abs() < 0.02);
        assert!(motion.open > 0.0 && motion.open < 1.0);
        for _ in 0..90 {
            step_gap(&mut motion, Some(2.0), 1.0 / 60.0, false);
        }
        assert!((motion.open - 1.0).abs() < 0.02);
        assert!(step_gap(&mut motion, Some(5.0), 1.0 / 60.0, false));
        assert!(motion.index > 2.0 && motion.index < 5.0);
        for _ in 0..90 {
            step_gap(&mut motion, Some(5.0), 1.0 / 60.0, false);
        }
        assert!((motion.index - 5.0).abs() < 0.02);
        motion.snap = true;
        assert!(!step_gap(&mut motion, None, 1.0 / 60.0, false));
        assert!(!motion.engaged);
    }

    #[test]
    fn gap_eases_shut_when_the_drag_leaves() {
        let mut motion = GapMotion::default();
        for _ in 0..90 {
            step_gap(&mut motion, Some(1.0), 1.0 / 60.0, false);
        }
        assert!(motion.engaged);
        assert!(step_gap(&mut motion, None, 1.0 / 60.0, false));
        assert!(motion.open < 1.0);
        for _ in 0..90 {
            step_gap(&mut motion, None, 1.0 / 60.0, false);
        }
        assert!(!motion.engaged);
    }
}
