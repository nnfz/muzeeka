use std::sync::Arc;
use std::time::Duration;

use gpui::{
    deferred, div, prelude::*, px, rgb, svg, uniform_list, Animation, AnimationExt, AnyElement,
    Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Rgba,
    SharedString, Styled,
};

use crate::ui::cover::cover_art;
use crate::ui::icons::{self, icon, icon_scaled};
use crate::ui::main_window::MainWindow;
use crate::ui::session::{self, SortColumn, Track, TrackId};
use crate::ui::theme;

const COL_INDEX: f32 = 36.;
pub(crate) const COL_ALBUM: f32 = 180.;
const COL_DURATION: f32 = 72.;
const COL_GAP: f32 = 6.;
/// Space after the index column. Smaller than the gap between the other columns.
const INDEX_GAP: f32 = 2.;
const LIKE_SLOT: f32 = 22.;
const ALBUM_MIN: f32 = 80.;
const ALBUM_MAX: f32 = 480.;
/// Gutter outside the painted track. The header uses the same left inset.
const SIDE_PAD: f32 = 8.;
const TRACK_ROW_H: f32 = 52.;
/// Top padding of the uniform list. The drop strip uses the same inset.
const TRACK_LIST_PAD: f32 = 8.;
/// Empty space inside the track, to the right of the duration digits.
const TRACK_END_PAD: f32 = 12.;
/// How often the visible playing row reads the mixer levels. The samples
/// themselves are measured on the audio thread. The library scan stays cached.
const EQ_FPS: f32 = 30.;

/// Cached rows for the open library view. Playback position is not part of it.
pub(crate) struct ListedTracks {
    view: session::LibraryView,
    sort: Option<session::Sort>,
    epoch: u64,
    indexes: Arc<Vec<usize>>,
    queue: Arc<Vec<TrackId>>,
}

impl MainWindow {
    pub(crate) fn visible_row(&self, id: TrackId) -> Option<usize> {
        self.listed
            .as_ref()
            .and_then(|listed| listed.queue.iter().position(|row| *row == id))
    }

    pub(crate) fn track_list(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, sort, view, epoch, playing, fresh) = {
            let session = self.session.read(cx);
            let view = session.view.clone();
            let sort = session.sort;
            let epoch = session.list_epoch();
            let hit = self.listed.as_ref().is_some_and(|listed| {
                listed.view == view && listed.sort == sort && listed.epoch == epoch
            });
            let fresh = if hit {
                None
            } else {
                let indexes = Arc::new(session.visible_indexes(""));
                let queue = Arc::new(
                    indexes
                        .iter()
                        .filter_map(|index| session.track_by_index(*index).map(|track| track.id))
                        .collect::<Vec<_>>(),
                );
                Some((indexes, queue))
            };
            (
                session.view_label(),
                sort,
                view,
                epoch,
                session.playing,
                fresh,
            )
        };
        if let Some((indexes, queue)) = fresh {
            self.listed = Some(ListedTracks {
                view,
                sort,
                epoch,
                indexes,
                queue,
            });
        }
        let (indexes, queue) = {
            let listed = self.listed.as_ref().expect("rows were just resolved");
            (listed.indexes.clone(), listed.queue.clone())
        };
        let count = indexes.len();
        self.set_drop_track_count(count);
        let gap = self.drop_gap();
        let gap_span = self.gap_span();
        let reorder_gap = self.reorder_gap();
        let list_len = count + usize::from(gap.is_some());

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .rounded_md()
            .bg(theme::bg_surface())
            .overflow_hidden()
            .child(self.track_drop_canvas(cx))
            .child(self.column_header(sort, cx))
            .child(if list_len == 0 {
                self.empty_tracks(&title).into_any_element()
            } else {
                self.track_rows(
                    indexes,
                    queue,
                    playing,
                    list_len,
                    gap,
                    gap_span,
                    reorder_gap,
                    cx,
                )
                .into_any_element()
            })
    }

    fn column_header(
        &self,
        sort: Option<session::Sort>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .group("track-header")
            .flex()
            .items_center()
            .h(px(36.))
            .pl(px(SIDE_PAD))
            .pr(px(SIDE_PAD + TRACK_END_PAD))
            .rounded_t_md()
            .bg(theme::bg_elevated())
            .border_b_1()
            .border_color(theme::border())
            .text_size(px(11.))
            .text_color(theme::text_muted())
            .child(
                div()
                    .w(px(COL_INDEX))
                    .mr(px(INDEX_GAP))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .child("#"),
            )
            .child(self.sort_header(
                "sort-title",
                "Title",
                SortColumn::Title,
                sort,
                true,
                COL_GAP,
                cx,
            ))
            .child(self.sort_header(
                "sort-album",
                "Album",
                SortColumn::Album,
                sort,
                false,
                COL_GAP,
                cx,
            ))
            .child(self.sort_header(
                "sort-duration",
                "Time",
                SortColumn::Duration,
                sort,
                false,
                0.,
                cx,
            ))
    }

    fn sort_header(
        &self,
        id: &'static str,
        label: &'static str,
        column: SortColumn,
        sort: Option<session::Sort>,
        grow: bool,
        trailing: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mark = sort.and_then(|sort| {
            (sort.column == column).then_some(if sort.descending { "↓" } else { "↑" })
        });
        let active = mark.is_some();
        let duration = column == SortColumn::Duration;
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_start()
            .text_left()
            .gap(px(4.))
            .when(duration, |cell| {
                cell.justify_end().text_right().group("sort-duration")
            })
            .h(px(24.))
            .rounded_sm()
            .cursor_pointer()
            .when(trailing > 0., |cell| cell.mr(px(trailing)))
            .when(grow, |cell| cell.flex_1().min_w(px(0.)))
            .when(column == SortColumn::Album, |cell| {
                cell.relative().flex_shrink_0().w(px(self.album_width))
            })
            .when(column == SortColumn::Duration, |cell| {
                cell.flex_shrink_0().w(px(COL_DURATION))
            })
            .text_color(if active {
                theme::text()
            } else {
                theme::text_muted()
            })
            .hover(|style| style.text_color(theme::text()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                    this.arm(id, false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if !this.armed(id) {
                        return;
                    }
                    this.session.update(cx, |session, cx| {
                        session.toggle_sort(column);
                        cx.notify();
                    });
                }),
            )
            .when_some(duration.then_some(mark).flatten(), |cell, mark| {
                cell.child(mark)
            })
            .when(column == SortColumn::Album, |cell| {
                cell.child(self.album_resize_handle(cx))
            })
            .child(if duration {
                svg()
                    .data(icons::TIME)
                    .w(px(14.))
                    .h(px(14.))
                    .flex_shrink_0()
                    .text_color(if active {
                        theme::text()
                    } else {
                        theme::text_muted()
                    })
                    .group_hover("sort-duration", |glyph| glyph.text_color(theme::text()))
                    .into_any_element()
            } else {
                label.into_any_element()
            })
            .when_some((!duration).then_some(mark).flatten(), |cell, mark| {
                cell.child(mark)
            })
            .into_any_element()
    }

    fn album_resize_handle(&self, cx: &mut Context<Self>) -> AnyElement {
        let grip = SharedString::from("album-resize");
        let resizing = self.album_resize.is_some();
        div()
            .id("album-resize")
            .group(grip.clone())
            .occlude()
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, false, window.mouse_position(), cx);
                }),
            )
            .absolute()
            .top(px(-6.))
            .left(px(-16.))
            .w(px(10.))
            .h(px(36.))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.album_resize = Some((event.position.x, this.album_width));
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(10.))
                    .right(px(1.))
                    .w(px(2.))
                    .h(px(16.))
                    .opacity(if resizing { 1. } else { 0. })
                    .group_hover("track-header", |line| line.opacity(1.))
                    .child(
                        div()
                            .size_full()
                            .rounded_full()
                            .bg(if resizing {
                                theme::accent()
                            } else {
                                gpui::rgba(0xffffff24)
                            })
                            .group_hover(grip, |line| line.bg(theme::accent())),
                    ),
            )
            .into_any_element()
    }

    pub(crate) fn resize_album(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some((origin_x, origin_width)) = self.album_resize else {
            return;
        };
        // The grip sits on the left edge, so dragging it right shrinks the column.
        let next = (origin_width - (x - origin_x) / px(1.)).clamp(ALBUM_MIN, ALBUM_MAX);
        if (self.album_width - next).abs() < 0.5 {
            return;
        }
        self.album_width = next;
        cx.notify();
    }

    fn empty_tracks(&self, title: &str) -> impl IntoElement {
        let heading = format!("{title} is empty");
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(16.))
                    .text_color(theme::text())
                    .child(heading),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child("Drop files or folders here"),
            )
    }

    fn track_rows(
        &mut self,
        indexes: Arc<Vec<usize>>,
        queue: Arc<Vec<TrackId>>,
        playing: bool,
        count: usize,
        gap: Option<(f32, f32)>,
        gap_span: f32,
        reorder_gap: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let this = cx.entity().clone();
        let scroll = self.track_scroll.0.borrow().base_handle.offset().y.as_f32();
        let rows_gap = gap;
        div()
            .flex_1()
            .min_h(px(0.))
            .relative()
            .overflow_hidden()
            .child(
                uniform_list("tracks", count, move |range, _window, app| {
                    let indexes = indexes.clone();
                    let queue = queue.clone();
                    let this = this.clone();
                    this.update(app, |view, cx| {
                        range
                            .filter_map(|row| {
                                // The extra cell is the room the shifted rows slide into.
                                if row >= indexes.len() {
                                    return Some(gap_spacer());
                                }
                                let index = *indexes.get(row)?;
                                let track = {
                                    let session = view.session.read(cx);
                                    session.track_by_index(index).cloned()
                                }?;
                                let shift = rows_gap
                                    .map(|(slot, open)| row_shift_by(row, slot, open, gap_span))
                                    .unwrap_or(0.);
                                Some(view.track_row(track, row, queue.clone(), playing, shift, cx))
                            })
                            .collect()
                    })
                })
                .py(px(8.))
                .size_full()
                .track_scroll(&self.track_scroll)
                .on_scroll_wheel(cx.listener(
                    |this, event: &gpui::ScrollWheelEvent, window, cx| {
                        this.note_scroll(
                            crate::ui::main_window::Scroller::Tracks,
                            event,
                            window,
                            cx,
                        );
                    },
                )),
            )
            .when_some(
                gap.filter(|(_, open)| *open > 0.001),
                |body, (slot, open)| {
                    body.child(drop_strip(slot, open, scroll, gap_span, reorder_gap))
                },
            )
            .child(self.scrollbar(crate::ui::main_window::Scroller::Tracks, cx))
    }

    fn track_row(
        &mut self,
        track: Track,
        index: usize,
        queue: Arc<Vec<TrackId>>,
        playing: bool,
        shift: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = track.id;
        let (current, source) = {
            let session = self.session.read(cx);
            (session.current == Some(id), session.view.clone())
        };
        let lifted = self
            .item_drag
            .as_ref()
            .is_some_and(|drag| drag.lifts_track(id));
        let held = lifted.then(|| {
            let scroll = -self.track_scroll.0.borrow().base_handle.offset().y.as_f32();
            self.item_drag
                .as_ref()
                .map(|drag| drag.hold_y(scroll))
                .unwrap_or(0.)
        });
        let reorder_px = held.unwrap_or_else(|| {
            self.item_drag
                .as_ref()
                .map(|drag| drag.track_row_delta(index) as f32 * TRACK_ROW_H)
                .unwrap_or(0.)
        });
        let export_path = std::path::PathBuf::from(crate::ui::menu::file_on_disk(&track.path));
        let is_playing = current && playing;
        let liked = track.liked;
        let group = SharedString::from(format!("row-{}", id.0));
        let row_id = SharedString::from(format!("track-row-{}", id.0));
        let play_id = row_id.clone();
        let like_id = SharedString::from(format!("like-{}", id.0));
        let hovered = self.hovered_track == Some(id);
        let hover_entity = cx.entity().clone();

        let row = div()
            .relative()
            .w_full()
            .h(px(TRACK_ROW_H))
            .min_h(px(TRACK_ROW_H))
            .max_h(px(TRACK_ROW_H))
            .child(div().w_full().h(px(TRACK_ROW_H)))
            .child(
                // The file-drop gap is already eased in `shift`. Neighbors glide into
                // the hole on a spring; the held row rides the pointer, unsprung.
                slide_box(
                    lifted,
                    shift,
                    reorder_px,
                    SharedString::from(format!("track-slide-{}", id.0)),
                    div()
                        .id(row_id)
                        .w_full()
                        .h_full()
                        .group(group.clone())
                        .flex()
                        .flex_1()
                        .min_w(px(0.))
                        .h_full()
                        .items_center()
                        .pr(px(TRACK_END_PAD))
                        .rounded_md()
                        .cursor_pointer()
                        .when(current, |row| row.bg(theme::accent_soft()))
                        .on_hover(move |over, _, app| {
                            hover_entity.update(app, |this, cx| {
                                // A press turns into a drag, and GPUI reports the row as
                                // unhovered the moment that happens. Keep the wash on the
                                // row the pointer went down on until it is released.
                                if !*over && this.press_at.is_some() {
                                    return;
                                }
                                if *over {
                                    if this.hovered_track != Some(id) {
                                        this.hovered_track = Some(id);
                                        cx.notify();
                                    }
                                } else if this.hovered_track == Some(id) {
                                    this.hovered_track = None;
                                    cx.notify();
                                }
                            });
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener({
                                let play_id = play_id.clone();
                                move |this, event: &gpui::MouseDownEvent, _, _cx| {
                                    this.press_at = Some(event.position);
                                    this.arm(play_id.clone(), false);
                                }
                            }),
                        )
                        .on_drag(
                            crate::ui::item_drag::TrackDrag {
                                ids: vec![id],
                                source,
                                paths: vec![export_path],
                                from_bar: false,
                            },
                            crate::ui::item_drag::spawn_track_ghost(),
                        )
                        .external_drag_payload(crate::ui::item_drag::file_drag_payload)
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                                if !this.armed(&play_id) {
                                    return;
                                }
                                let queue = queue.as_ref().clone();
                                this.session.update(cx, |session, cx| {
                                    session.play(id, queue);
                                    cx.notify();
                                });
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_track_menu(id, event.position, window, cx);
                            }),
                        )
                        .child(self.index_cell(
                            index,
                            current,
                            is_playing,
                            cx.reduce_motion(),
                            group,
                        ))
                        .child(self.title_cell(&track, current))
                        .child(
                            div()
                                .w(px(self.album_width))
                                .h(px(14.))
                                .mr(px(COL_GAP))
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .justify_start()
                                .text_left()
                                .text_size(px(12.))
                                .line_height(px(14.))
                                .text_color(theme::text_muted())
                                .whitespace_nowrap()
                                .truncate()
                                .child(track.album.clone()),
                        )
                        .child(self.duration_cell(like_id, liked, id, track.duration, cx))
                        .with_spring(
                            SharedString::from(format!("track-hover-{}", id.0)),
                            MainWindow::hover_spring(hovered),
                            move |row, phase| {
                                if current {
                                    row
                                } else {
                                    row.bg(theme::hover().opacity(phase.0))
                                }
                            },
                        ),
                ),
            );
        if lifted {
            deferred(row).with_priority(1).into_any_element()
        } else {
            row.into_any_element()
        }
    }

    fn index_cell(
        &self,
        index: usize,
        current: bool,
        playing: bool,
        reduce_motion: bool,
        group: SharedString,
    ) -> AnyElement {
        div()
            .w(px(COL_INDEX))
            .mr(px(INDEX_GAP))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(12.))
            .text_color(if current {
                theme::accent()
            } else {
                theme::text_muted()
            })
            .child(if playing {
                eq_bars("track-eq", reduce_motion)
            } else if current {
                icon(icons::PAUSE, theme::accent(), px(12.)).into_any_element()
            } else {
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .group_hover(group.clone(), |style| style.invisible())
                            .child(format!("{}", index + 1)),
                    )
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .invisible()
                            .group_hover(group, |style| style.visible())
                            .child(icon(icons::PLAY, theme::text(), px(12.))),
                    )
                    .into_any_element()
            })
            .into_any_element()
    }

    fn title_cell(&self, track: &Track, current: bool) -> AnyElement {
        // Cached `.ttml` is loaded with the track. This window does not draw a
        // lyric view yet; the read keeps that path on the rendered row.
        let _lyric_file = track.lyrics.as_deref();
        div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .mr(px(COL_GAP))
            .items_center()
            .gap(px(10.))
            .child(cover_art(track.cover.as_deref(), 40.))
            .child(
                // Both lines keep a fixed box. An empty artist, a wrapped
                // title, or a fallback font must not move the pair in the row.
                div()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .top(px(1.))
                    .gap(px(2.))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(
                        div()
                            .h(px(15.))
                            .flex_shrink_0()
                            .text_size(px(13.))
                            .line_height(px(15.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(if current {
                                theme::accent()
                            } else {
                                theme::text()
                            })
                            .whitespace_nowrap()
                            .truncate()
                            .child(track.title.clone()),
                    )
                    .child(
                        div()
                            .h(px(14.))
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .line_height(px(14.))
                            .text_color(theme::text_muted())
                            .whitespace_nowrap()
                            .truncate()
                            .child(track.artist.clone()),
                    ),
            )
            .into_any_element()
    }

    fn duration_cell(
        &self,
        like_id: SharedString,
        liked: bool,
        id: TrackId,
        duration: f64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w(px(COL_DURATION))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(COL_GAP))
            .child(self.like_button(
                like_id,
                liked,
                id,
                LIKE_SLOT,
                14.,
                theme::text_muted(),
                true,
                cx,
            ))
            .child(
                div()
                    .flex_shrink_0()
                    .text_right()
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child(session::format_time(duration)),
            )
            .into_any_element()
    }

    pub(crate) fn like_button(
        &self,
        id: SharedString,
        liked: bool,
        track_id: TrackId,
        slot: f32,
        icon_size: f32,
        idle: Rgba,
        stop_row_click: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let spring_id = SharedString::from(format!("{id}-fill"));
        let hot_id = SharedString::from(format!("{id}-hot"));
        let hovered = self.hovered_ui.as_ref() == Some(&id);
        let glyphs = move |scale: f32| {
            let spring_id = spring_id.clone();
            let hot_id = hot_id.clone();
            div().relative().size(px(icon_size)).with_spring(
                spring_id,
                MainWindow::hover_spring(liked),
                move |stack, phase| {
                    let t = phase.0.clamp(0.0, 1.0);
                    let hot_id = hot_id.clone();
                    stack.child(div().absolute().inset_0().with_spring(
                        hot_id,
                        MainWindow::hover_spring(hovered),
                        move |layer, hot| {
                            // Idle outline and idle fill share one gray. Hover
                            // tints whichever glyph is showing.
                            let color = mix_rgba(idle, rgb(0xe25555), hot.0);
                            layer
                                .child(like_glyph(icons::HEART, color, icon_size, 1.0 - t, scale))
                                .child(like_glyph(icons::HEART_FILLED, color, icon_size, t, scale))
                        },
                    ))
                },
            )
        };
        let button_id = id.clone();
        let release_id = id.clone();
        let hover_id = id.clone();
        let press_id = SharedString::from(format!("{id}-press"));
        let pressed = self.pressed_ui.as_ref() == Some(&id);
        // List hearts stop the row from arming. The like itself waits for
        // release. on_click is avoided because it clears the hover wash.
        let on_down = cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
            if stop_row_click {
                cx.stop_propagation();
            }
            let shrink = !stop_row_click;
            this.arm(id.clone(), shrink);
            if shrink {
                cx.notify();
            }
        });
        let on_release = cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
            if !this.armed(&release_id) {
                return;
            }
            this.session.update(cx, |session, cx| {
                session.toggle_like(track_id);
                cx.notify();
            });
        });
        let button = div()
            .id(button_id)
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(px(slot))
            .cursor_pointer()
            .on_hover(Self::hover_listener(hover_id, cx))
            .on_mouse_down(MouseButton::Left, on_down)
            .on_mouse_up(MouseButton::Left, on_release);
        // List rows only toggle. The transport heart also shrinks on press.
        if stop_row_click {
            button.rounded_sm().child(glyphs(1.0)).into_any_element()
        } else {
            button
                .child(
                    div()
                        .size(px(slot))
                        .flex()
                        .items_center()
                        .justify_center()
                        .with_spring(
                            press_id,
                            Self::hover_spring(pressed),
                            move |frame, phase| frame.child(glyphs(Self::press_scale(phase))),
                        ),
                )
                .into_any_element()
        }
    }
}

fn mix_rgba(from: Rgba, to: Rgba, t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    Rgba {
        r: from.r + (to.r - from.r) * t,
        g: from.g + (to.g - from.g) * t,
        b: from.b + (to.b - from.b) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

fn like_glyph(
    glyph: &'static [u8],
    color: gpui::Rgba,
    size: f32,
    opacity: f32,
    scale: f32,
) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .opacity(opacity)
        .child(if scale == 1.0 {
            icon(glyph, color, px(size)).into_any_element()
        } else {
            icon_scaled(glyph, color, px(size), scale).into_any_element()
        })
        .into_any_element()
}

/// How far a row slides down while the gap sits at `slot`. `open` is 0..1.
#[cfg(test)]
fn row_shift(row: usize, slot: f32, open: f32) -> f32 {
    row_shift_by(row, slot, open, TRACK_ROW_H)
}

fn row_shift_by(row: usize, slot: f32, open: f32, span: f32) -> f32 {
    let along = (1.0 - (slot - row as f32)).clamp(0.0, 1.0);
    span * along * open.clamp(0.0, 1.0)
}

/// Box a track row slides in. Neighbors ease toward their slot; the row under
/// the pointer jumps straight there so the grab point never lags the cursor.
fn slide_box(
    held: bool,
    shift: f32,
    reorder_px: f32,
    spring: SharedString,
    row: impl IntoElement,
) -> AnyElement {
    let frame = div()
        .absolute()
        .left(px(SIDE_PAD))
        .right(px(SIDE_PAD))
        .h(px(TRACK_ROW_H))
        .child(row);
    if held {
        frame.top(px(shift + reorder_px)).into_any_element()
    } else {
        frame
            .top(px(shift))
            .with_spring(
                spring,
                MainWindow::smooth_spring_px(reorder_px),
                move |row, extra| {
                    if extra == px(0.) {
                        row
                    } else {
                        row.top(px(shift) + extra)
                    }
                },
            )
            .into_any_element()
    }
}

fn gap_spacer() -> AnyElement {
    div().w_full().h(px(TRACK_ROW_H)).into_any_element()
}

fn drop_strip(slot: f32, open: f32, scroll: f32, span: f32, reorder: bool) -> AnyElement {
    let open = open.clamp(0.0, 1.0);
    let wash = if reorder {
        div()
            .size_full()
            .rounded_md()
            .bg(theme::accent_soft())
            .border_1()
            .border_color(theme::accent().alpha(0.28))
            .opacity(open)
            .into_any_element()
    } else {
        div()
            .size_full()
            .rounded_md()
            .border_1()
            .border_dashed()
            .border_color(theme::accent())
            .bg(theme::accent_soft())
            .opacity(open)
            .into_any_element()
    };
    div()
        .absolute()
        .left(px(0.))
        .right(px(0.))
        .top(px(TRACK_LIST_PAD + slot * TRACK_ROW_H + scroll))
        .h(px(span * open))
        .px(px(SIDE_PAD))
        .py(px(8. * open))
        .child(wash)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::row_shift;

    fn tone(hz: f32, seconds: f32) -> Vec<f32> {
        let rate = 48_000.0f32;
        let frames = (rate * seconds) as usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let sample = (i as f32 * hz * std::f32::consts::TAU / rate).sin();
            samples.push(sample);
            samples.push(sample);
        }
        samples
    }

    #[test]
    fn bass_lifts_the_low_bar_and_treble_lifts_the_high_one() {
        let mut bass = muzeeka::audio::levels::BandMeter::new(48_000.0);
        let low = bass.push_stereo(&tone(60.0, 0.3));
        assert!(low[0] > 0.65, "low {low:?}");
        assert!(low[1] < 0.25 && low[2] < 0.2, "low leak {low:?}");

        // A clap / pluck cracks around here. It must leave the bass bar alone.
        let mut hit = muzeeka::audio::levels::BandMeter::new(48_000.0);
        let mid = hit.push_stereo(&tone(2_000.0, 0.3));
        assert!(mid[1] > 0.65, "mid {mid:?}");
        assert!(
            mid[0] < 0.15 && mid[1] > mid[2],
            "hit on the bass bar {mid:?}"
        );

        let mut air = muzeeka::audio::levels::BandMeter::new(48_000.0);
        let high = air.push_stereo(&tone(10_000.0, 0.3));
        assert!(high[2] > 0.65, "high {high:?}");
        assert!(high[0] < 0.15 && high[2] > high[1], "high leak {high:?}");
    }

    #[test]
    fn bars_wait_out_the_output_buffer() {
        let mut meter = muzeeka::audio::levels::BandMeter::new(48_000.0);
        meter.set_hear_delay_secs(0.20);
        let early = meter.push_stereo(&tone(60.0, 0.12));
        assert!(early[0] < 0.05, "early {early:?}");
        let heard = meter.push_stereo(&tone(60.0, 0.25));
        assert!(heard[0] > 0.5, "heard {heard:?}");
    }

    #[test]
    fn row_shift_follows_the_gap() {
        assert_eq!(row_shift(0, 2.0, 1.0), 0.0);
        assert_eq!(row_shift(1, 2.5, 1.0), 0.0);
        assert_eq!(row_shift(2, 2.0, 1.0), 52.0);
        assert_eq!(row_shift(3, 2.0, 1.0), 52.0);
        assert!((row_shift(2, 2.5, 1.0) - 26.0).abs() < 0.01);
        assert!((row_shift(2, 2.0, 0.5) - 26.0).abs() < 0.01);
        assert_eq!(row_shift(4, 2.5, 1.0), 52.0);
    }
}

fn eq_bar(level: f32) -> AnyElement {
    let level = level.clamp(0.0, 1.0);
    // A short rest so the mark stays visible in a quiet moment.
    let height = 2.0 + 12.0 * level;
    div()
        .w(px(3.))
        .h(px(height))
        .flex_shrink_0()
        .rounded(px(1.))
        .bg(theme::accent())
        .into_any_element()
}

fn paint_levels(row: gpui::Div) -> gpui::Div {
    let levels = muzeeka::audio::levels::levels();
    row.children(levels.map(eq_bar))
}

pub(crate) fn eq_bars(id: impl Into<gpui::ElementId>, reduce_motion: bool) -> AnyElement {
    let row = div()
        .flex()
        .items_end()
        .justify_center()
        .gap(px(2.))
        .h(px(14.));
    if reduce_motion {
        return paint_levels(row).into_any_element();
    }
    // The phase is unused. The animation only asks for the next paint so the
    // bars can follow the mixer. Levels are already computed on the audio thread.
    row.with_animation(
        id,
        Animation::new(Duration::from_millis(1_000))
            .repeat()
            .with_max_fps(EQ_FPS),
        |row, _phase| paint_levels(row),
    )
    .into_any_element()
}
