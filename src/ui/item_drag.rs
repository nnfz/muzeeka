use std::path::PathBuf;

use gpui::{
    canvas, div, point, prelude::*, px, App, Context, CursorStyle, DragMoveEvent, Entity,
    ExternalDragPayload, FileDragPaths, IntoElement, Pixels, Point, Render, Window,
};

use crate::ui::main_window::MainWindow;
use crate::ui::reorder::{playlist_slot, slot_from_y, stabilize_slot};
use crate::ui::session::{LibraryView, TrackId};

pub(crate) const REORDER_GAP: f32 = 44.;

const TRACK_THRESHOLD: f32 = 4.;
const PLAYLIST_THRESHOLD: f32 = 5.;
const TRACK_ROW_H: f32 = 52.;
const TRACK_HEADER_H: f32 = 36.;
const TRACK_LIST_PAD: f32 = 8.;
const TRACK_HYSTERESIS: f32 = 14.;
const PLAYLIST_HYSTERESIS: f32 = 4.;
const PLAYLIST_SLACK: f32 = 60.;
const AUTOSCROLL_EDGE: f32 = 32.;
const AUTOSCROLL_MAX: f32 = 18.;

#[derive(Clone)]
pub(crate) struct TrackDrag {
    pub ids: Vec<TrackId>,
    pub source: LibraryView,
    pub paths: Vec<PathBuf>,
    pub from_bar: bool,
}

#[derive(Clone)]
pub(crate) struct PlaylistDrag {
    pub id: String,
}

enum DragKind {
    Tracks {
        ids: Vec<TrackId>,
        source: LibraryView,
    },
    Playlist {
        id: String,
    },
}

pub(crate) struct ItemDrag {
    kind: DragKind,
    pointer: Point<Pixels>,
    anchor: Point<Pixels>,
    anchor_scroll: f32,
    paths: Vec<PathBuf>,
    shown: bool,
    floating: bool,
    copy: bool,
    track_slot: Option<usize>,
    track_from: Option<usize>,
    playlist_slot: Option<usize>,
    playlist_slots: Vec<f32>,
    playlist_target: Option<String>,
}

impl ItemDrag {
    fn tracks(ids: Vec<TrackId>, source: LibraryView, paths: Vec<PathBuf>) -> Self {
        Self {
            kind: DragKind::Tracks { ids, source },
            pointer: point(px(0.), px(0.)),
            anchor: point(px(0.), px(0.)),
            anchor_scroll: 0.,
            paths,
            shown: false,
            floating: false,
            copy: false,
            track_slot: None,
            track_from: None,
            playlist_slot: None,
            playlist_slots: Vec::new(),
            playlist_target: None,
        }
    }

    fn playlist(id: String) -> Self {
        Self {
            kind: DragKind::Playlist { id },
            pointer: point(px(0.), px(0.)),
            anchor: point(px(0.), px(0.)),
            anchor_scroll: 0.,
            paths: Vec::new(),
            shown: false,
            floating: false,
            copy: false,
            track_slot: None,
            track_from: None,
            playlist_slot: None,
            playlist_slots: Vec::new(),
            playlist_target: None,
        }
    }

    fn holds_track(&self, id: TrackId) -> bool {
        matches!(&self.kind, DragKind::Tracks { ids, .. } if ids.contains(&id))
    }

    pub(crate) fn lifts_track(&self, id: TrackId) -> bool {
        self.shown && !self.floating && self.track_slot.is_some() && self.holds_track(id)
    }

    pub(crate) fn shows_ghost(&self) -> bool {
        self.shown && self.floating
    }

    pub(crate) fn track_row_delta(&self, row: usize) -> i32 {
        if !self.shown || self.floating || self.copy {
            return 0;
        }
        let (Some(slot), Some(from)) = (self.track_slot, self.track_from) else {
            return 0;
        };
        if row == from {
            return 0;
        }
        crate::ui::reorder::single_row_shift(row, from, slot)
    }

    pub(crate) fn playlist_id(&self) -> Option<&str> {
        match &self.kind {
            DragKind::Playlist { id } => Some(id.as_str()),
            DragKind::Tracks { .. } => None,
        }
    }

    pub(crate) fn playlist_slide(&self, index: usize, count: usize, from: usize) -> f32 {
        if !self.shown || !self.is_playlist() || self.playlist_slots.len() != count {
            return 0.;
        }
        let Some(slot) = self.playlist_slot else {
            return 0.;
        };
        if index >= count || from >= count || index == from {
            return 0.;
        }
        let to = crate::ui::reorder::index_after_move(index, from, slot, count);
        self.playlist_slots
            .get(to)
            .copied()
            .unwrap_or(self.playlist_slots[index])
            - self.playlist_slots[index]
    }

    pub(crate) fn lifts_playlist(&self, id: &str) -> bool {
        self.shown && matches!(&self.kind, DragKind::Playlist { id: dragged } if dragged == id)
    }

    pub(crate) fn hold_y(&self, scroll: f32) -> f32 {
        self.pointer.y.as_f32() - self.anchor.y.as_f32() + scroll - self.anchor_scroll
    }

    pub(crate) fn targets_playlist(&self, id: &str) -> bool {
        self.shown && self.playlist_target.as_deref() == Some(id)
    }

    fn same_tracks(&self, ids: &[TrackId]) -> bool {
        matches!(&self.kind, DragKind::Tracks { ids: dragged, .. } if dragged == ids)
    }

    fn same_playlist(&self, id: &str) -> bool {
        matches!(&self.kind, DragKind::Playlist { id: dragged } if dragged == id)
    }

    fn is_playlist(&self) -> bool {
        matches!(self.kind, DragKind::Playlist { .. })
    }
}

pub(crate) struct PlaylistGhost;

impl Render for PlaylistGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(0.)).h(px(0.))
    }
}

pub(crate) fn spawn_playlist_ghost(
) -> impl Fn(&PlaylistDrag, Point<Pixels>, &mut Window, &mut App) -> Entity<PlaylistGhost> + 'static
{
    |_, _, _, app| app.new(|_| PlaylistGhost)
}

pub(crate) struct TrackGhost;

impl Render for TrackGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(0.)).h(px(0.))
    }
}

pub(crate) fn spawn_track_ghost(
) -> impl Fn(&TrackDrag, Point<Pixels>, &mut Window, &mut App) -> Entity<TrackGhost> + 'static {
    |_, _, _, app| app.new(|_| TrackGhost)
}

pub(crate) fn file_drag_payload(
    drag: &TrackDrag,
    _: &mut Window,
    _: &mut App,
) -> Option<ExternalDragPayload> {
    let entries: Vec<_> = drag
        .paths
        .iter()
        .filter(|path| path.is_file())
        .map(|path| (path.clone(), false))
        .collect();
    if entries.is_empty() {
        None
    } else {
        Some(ExternalDragPayload::Files(FileDragPaths::new(entries)))
    }
}

impl MainWindow {
    pub(crate) fn note_track_drag(
        &mut self,
        event: &DragMoveEvent<TrackDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dragged = event.drag(cx).clone();
        let position = event.event.position;
        let copy = event.event.modifiers.control || event.event.modifiers.platform;
        let origin = self.press_at.unwrap_or(position);
        let moved = travelled(origin, position);
        if !self
            .item_drag
            .as_ref()
            .is_some_and(|drag| drag.same_tracks(&dragged.ids))
        {
            self.item_drag = Some(ItemDrag::tracks(
                dragged.ids.clone(),
                dragged.source.clone(),
                dragged.paths.clone(),
            ));
        }
        let already_shown = self.item_drag.as_ref().is_some_and(|drag| drag.shown);
        let show_now = !already_shown && moved >= TRACK_THRESHOLD;
        if show_now {
            self.armed_ui = None;
            self.dismiss_menu(window, cx);
        }
        let shown = already_shown || show_now;
        let in_window = pointer_inside(window, position);
        if shown
            && (event.event.modifiers.alt || !in_window)
            && dragged.paths.iter().any(|path| path.is_file())
        {
            self.hand_tracks_out(dragged.paths.clone(), window, cx);
            return;
        }
        let in_tracks = in_window && self.pointer_in_tracks(position);
        let floating = shown && (dragged.from_bar || !in_tracks);
        let prev = self.item_drag.as_ref().and_then(|drag| drag.track_slot);
        let (target, slot) = if !shown || !in_window {
            (None, None)
        } else if dragged.from_bar || !in_tracks {
            (self.playlist_target_at(position, &dragged.source), None)
        } else {
            self.track_drop_target(position, &dragged.source, copy, prev)
        };
        let from_row = dragged
            .ids
            .first()
            .copied()
            .and_then(|id| self.visible_row(id));
        let track_scroll = self.track_scroll_y();
        if let Some(drag) = self.item_drag.as_mut() {
            if show_now {
                drag.anchor = position;
                drag.anchor_scroll = track_scroll;
            }
            drag.pointer = position;
            drag.shown = shown;
            drag.floating = floating;
            drag.copy = copy;
            drag.playlist_target = target;
            drag.track_slot = slot;
            drag.track_from = from_row;
        }
    }

    pub(crate) fn note_playlist_drag(
        &mut self,
        event: &DragMoveEvent<PlaylistDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = event.drag(cx).id.clone();
        let position = event.event.position;
        let origin = self.press_at.unwrap_or(position);
        let travelled = travelled(origin, position);
        if !self
            .item_drag
            .as_ref()
            .is_some_and(|drag| drag.same_playlist(&id))
        {
            self.item_drag = Some(ItemDrag::playlist(id));
        }
        let already_shown = self.item_drag.as_ref().is_some_and(|drag| drag.shown);
        let show_now = !already_shown && travelled >= PLAYLIST_THRESHOLD;
        let centers = if show_now {
            self.playlist_slot_centers()
        } else {
            self.item_drag
                .as_ref()
                .map(|drag| drag.playlist_slots.clone())
                .unwrap_or_default()
        };
        if show_now {
            self.armed_ui = None;
            self.dismiss_menu(window, cx);
        }
        let shown = already_shown || show_now;
        let prev = self.item_drag.as_ref().and_then(|drag| drag.playlist_slot);
        let slot = if shown {
            self.playlist_drop_index(position, &centers, prev)
        } else {
            None
        };
        let sidebar_scroll = self.sidebar_scroll_y();
        if let Some(drag) = self.item_drag.as_mut() {
            if show_now {
                drag.anchor = position;
                drag.anchor_scroll = sidebar_scroll;
                drag.playlist_slots = centers;
            }
            drag.pointer = position;
            drag.shown = shown;
            drag.playlist_slot = slot;
        }
    }

    pub(crate) fn finish_track_drag(&mut self, _: &TrackDrag, cx: &mut Context<Self>) {
        self.release_pointer_gesture();
        self.set_reorder_gap(None);
        let Some(item) = self.item_drag.take() else {
            return;
        };
        if !item.shown {
            return;
        }
        let DragKind::Tracks { ids, source } = item.kind else {
            return;
        };
        if let Some(target) = item.playlist_target.clone() {
            let name = {
                let session = self.session.read(cx);
                session
                    .playlists
                    .iter()
                    .find(|playlist| playlist.id == target)
                    .map(|playlist| playlist.name.clone())
                    .unwrap_or_else(|| "Playlist".to_string())
            };
            let copy = item.copy;
            let count = self.session.update(cx, |session, cx| {
                let count = if copy {
                    session.copy_tracks(&ids, &target)
                } else {
                    session.move_tracks(&ids, &target, &source)
                };
                cx.notify();
                count
            });
            if count > 0 {
                let verb = if copy { "Copied" } else { "Moved" };
                let noun = if count == 1 { "track" } else { "tracks" };
                self.show_drop_toast(format!("{verb} {count} {noun} to {name}"), false);
            }
            cx.notify();
            return;
        }
        if item.copy {
            return;
        }
        let Some(slot) = item.track_slot else {
            return;
        };
        if self.session.read(cx).view != source {
            return;
        }
        let changed = self.session.update(cx, |session, cx| {
            let changed = session.reorder_view(&ids, slot);
            if changed {
                cx.notify();
            }
            changed
        });
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn finish_playlist_drag(&mut self, _: &PlaylistDrag, cx: &mut Context<Self>) {
        self.release_pointer_gesture();
        let Some(item) = self.item_drag.take() else {
            return;
        };
        if !item.shown {
            return;
        }
        let DragKind::Playlist { id } = item.kind else {
            return;
        };
        let Some(slot) = item.playlist_slot else {
            return;
        };
        let changed = self.session.update(cx, |session, cx| {
            let changed = session.reorder_playlists(&[id], slot);
            if changed {
                cx.notify();
            }
            changed
        });
        if changed {
            cx.notify();
        }
    }

    pub(crate) fn tick_playlist_autoscroll(&mut self, window: &mut Window) {
        let Some(pointer) = self
            .item_drag
            .as_ref()
            .filter(|drag| drag.shown && drag.is_playlist())
            .map(|drag| drag.pointer)
        else {
            return;
        };
        let bounds = self.playlist_list_bounds();
        if bounds.size.height <= px(1.) {
            return;
        }
        let above = bounds.top().as_f32() + AUTOSCROLL_EDGE - pointer.y.as_f32();
        let below = pointer.y.as_f32() - (bounds.bottom().as_f32() - AUTOSCROLL_EDGE);
        let delta = if above > 0. {
            scroll_speed(above)
        } else if below > 0. {
            -scroll_speed(below)
        } else {
            return;
        };
        if !self.scroll_sidebar_by(delta) {
            return;
        }
        let centers = self
            .item_drag
            .as_ref()
            .map(|drag| drag.playlist_slots.clone())
            .unwrap_or_default();
        let prev = self.item_drag.as_ref().and_then(|drag| drag.playlist_slot);
        let slot = self.playlist_drop_index(pointer, &centers, prev);
        if let Some(drag) = self.item_drag.as_mut() {
            drag.playlist_slot = slot;
        }
        window.request_animation_frame();
    }

    pub(crate) fn hand_held_tracks_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paths) = self
            .item_drag
            .as_ref()
            .and_then(|drag| (drag.shown && !drag.paths.is_empty()).then(|| drag.paths.clone()))
        else {
            return;
        };
        if paths.iter().any(|path| path.is_file()) {
            self.hand_tracks_out(paths, window, cx);
        }
    }

    pub(crate) fn hand_tracks_out(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let armed = crate::ui::file_drag_out::try_begin();
        self.item_drag = None;
        self.release_pointer_gesture();
        self.set_reorder_gap(None);
        window.release_pointer();
        cx.stop_active_drag(window);
        if armed {
            cx.spawn(async move |_this, _cx| {
                crate::ui::file_drag_out::run_armed(&paths);
            })
            .detach();
        }
    }

    pub(crate) fn retire_item_drag_if_finished(&mut self, cx: &gpui::App) {
        if self.item_drag.is_none() || cx.has_active_drag() {
            return;
        }
        self.item_drag = None;
        self.release_pointer_gesture();
        self.set_reorder_gap(None);
    }

    fn pointer_in_tracks(&self, position: Point<Pixels>) -> bool {
        let bounds = self.track_panel_bounds();
        bounds.size.height > px(1.) && bounds.contains(&position)
    }

    fn playlist_target_at(&self, position: Point<Pixels>, source: &LibraryView) -> Option<String> {
        let id = self.playlist_id_at(position)?;
        let same = matches!(source, LibraryView::Playlist(open) if open == &id);
        if same {
            None
        } else {
            Some(id)
        }
    }

    fn track_drop_target(
        &self,
        position: Point<Pixels>,
        source: &LibraryView,
        copy: bool,
        prev: Option<usize>,
    ) -> (Option<String>, Option<usize>) {
        if let Some(id) = self.playlist_id_at(position) {
            let same = matches!(source, LibraryView::Playlist(open) if open == &id);
            if same {
                return (None, None);
            }
            return (Some(id), None);
        }
        if copy {
            return (None, None);
        }
        let bounds = self.track_panel_bounds();
        if bounds.size.height <= px(1.) || !bounds.contains(&position) {
            return (None, None);
        }
        let count = self.drop_track_count();
        if count == 0 {
            return (None, None);
        }
        let scroll = -self.track_scroll.0.borrow().base_handle.offset().y.as_f32();
        let y =
            position.y.as_f32() - bounds.top().as_f32() - TRACK_HEADER_H - TRACK_LIST_PAD + scroll;
        if y < -4. {
            return (None, None);
        }
        let raw = slot_from_y(y, count, TRACK_ROW_H);
        let slot = stabilize_slot(raw, prev, y, TRACK_ROW_H, TRACK_HYSTERESIS);
        (None, Some(slot))
    }

    fn playlist_drop_index(
        &self,
        position: Point<Pixels>,
        centers: &[f32],
        prev: Option<usize>,
    ) -> Option<usize> {
        if centers.is_empty() {
            return None;
        }
        let bounds = self.playlist_list_bounds();
        if bounds.size.height <= px(1.) {
            return None;
        }
        let y = position.y.as_f32();
        let top = bounds.top().as_f32();
        let bottom = bounds.bottom().as_f32();
        if y < top - PLAYLIST_SLACK || y > bottom + PLAYLIST_SLACK {
            return None;
        }
        let scroll = -self.sidebar_scroll.offset().y.as_f32();
        Some(playlist_slot(
            y - top + scroll,
            centers,
            prev,
            PLAYLIST_HYSTERESIS,
        ))
    }
}

impl MainWindow {
    fn track_scroll_y(&self) -> f32 {
        -self.track_scroll.0.borrow().base_handle.offset().y.as_f32()
    }

    fn sidebar_scroll_y(&self) -> f32 {
        -self.sidebar_scroll.offset().y.as_f32()
    }
}

fn travelled(from: Point<Pixels>, to: Point<Pixels>) -> f32 {
    let dx = (to.x - from.x).as_f32();
    let dy = (to.y - from.y).as_f32();
    dx.hypot(dy)
}

fn pointer_inside(window: &Window, position: Point<Pixels>) -> bool {
    let size = window.viewport_size();
    position.x > px(0.)
        && position.y > px(0.)
        && position.x < size.width
        && position.y < size.height
}

fn scroll_speed(distance: f32) -> f32 {
    (2. + distance / 3.).min(AUTOSCROLL_MAX)
}

pub(crate) fn drag_cursor_layer(host: Entity<MainWindow>) -> impl IntoElement {
    canvas(
        |_, _, _| host,
        |_, host, window, cx| {
            let copying = host
                .read(cx)
                .item_drag
                .as_ref()
                .is_some_and(|drag| drag.shows_ghost());
            if !crate::ui::copy_cursor::set(window, copying) && copying {
                window.set_window_cursor_style(CursorStyle::DragCopy);
            }
        },
    )
    .absolute()
    .size_full()
}
