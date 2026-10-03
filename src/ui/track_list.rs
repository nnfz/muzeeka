use std::sync::Arc;

use gpui::{
    div, prelude::*, px, rgb, svg, uniform_list, AnimationExt, AnyElement, Context,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Rgba, SharedString, Styled,
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
/// Empty space inside the track, to the right of the duration digits.
const TRACK_END_PAD: f32 = 12.;

impl MainWindow {
    pub(crate) fn track_list(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, sort, indexes, queue, playing, position) = {
            let session = self.session.read(cx);
            let title = session.view_label();
            let sort = session.sort;
            let indexes = session.visible_indexes("");
            let queue: Vec<TrackId> = indexes
                .iter()
                .filter_map(|index| session.track_by_index(*index).map(|track| track.id))
                .collect();
            (
                title,
                sort,
                indexes,
                queue,
                session.playing,
                session.position,
            )
        };
        let count = indexes.len();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .rounded_md()
            .bg(theme::bg_surface())
            .overflow_hidden()
            .child(self.column_header(sort, cx))
            .child(if count == 0 {
                self.empty_tracks(&title).into_any_element()
            } else {
                self.track_rows(indexes, queue, playing, position, count, cx)
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
        indexes: Vec<usize>,
        queue: Vec<TrackId>,
        playing: bool,
        position: f64,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let this = cx.entity().clone();
        let indexes = Arc::new(indexes);
        let queue = Arc::new(queue);
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
                            let index = *indexes.get(row)?;
                            let track = {
                                let session = view.session.read(cx);
                                session.track_by_index(index).cloned()
                            }?;
                            Some(view.track_row(track, row, queue.clone(), playing, position, cx))
                        })
                        .collect()
                })
            })
            .py(px(8.))
            .size_full()
            .track_scroll(&self.track_scroll)
            .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, window, cx| {
                this.note_scroll(crate::ui::main_window::Scroller::Tracks, event, window, cx);
            })),
            )
            .child(self.scrollbar(crate::ui::main_window::Scroller::Tracks, cx))
    }

    fn track_row(
        &mut self,
        track: Track,
        index: usize,
        queue: Arc<Vec<TrackId>>,
        playing: bool,
        position: f64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = self.session.read(cx).current == Some(track.id);
        let is_playing = current && playing;
        let liked = track.liked;
        let id = track.id;
        let group = SharedString::from(format!("row-{}", id.0));
        let row_id = SharedString::from(format!("track-row-{}", id.0));
        let play_id = row_id.clone();
        let like_id = SharedString::from(format!("like-{}", id.0));
        let hovered = self.hovered_track == Some(id);
        let hover_entity = cx.entity().clone();

        div()
            .flex()
            .w_full()
            .h(px(52.))
            .px(px(SIDE_PAD))
            .child(
                div()
                    .id(row_id)
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
                            move |this, _: &gpui::MouseDownEvent, _, _cx| {
                                this.arm(play_id.clone(), false);
                            }
                        }),
                    )
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
                    .child(self.index_cell(index, current, is_playing, position, group))
                    .child(self.title_cell(&track, current))
                    .child(
                        div()
                            .w(px(self.album_width))
                            .mr(px(COL_GAP))
                            .flex_shrink_0()
                            .flex()
                            .justify_start()
                            .text_left()
                            .text_size(px(12.))
                            .text_color(theme::text_secondary())
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
            )
            .into_any_element()
    }

    fn index_cell(
        &self,
        index: usize,
        current: bool,
        playing: bool,
        position: f64,
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
                eq_bars(position).into_any_element()
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
                div()
                    .flex()
                    .flex_col()
                    .top(px(1.))
                    .gap(px(2.))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(15.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(if current {
                                theme::accent()
                            } else {
                                theme::text()
                            })
                            .truncate()
                            .child(track.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(14.))
                            .text_color(theme::text_muted())
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
                                .child(like_glyph(
                                    icons::HEART,
                                    color,
                                    icon_size,
                                    1.0 - t,
                                    scale,
                                ))
                                .child(like_glyph(
                                    icons::HEART_FILLED,
                                    color,
                                    icon_size,
                                    t,
                                    scale,
                                ))
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

fn eq_bars(position: f64) -> impl IntoElement {
    div()
        .flex()
        .items_end()
        .justify_center()
        .gap(px(2.))
        .h(px(12.))
        .children([0.0, 1.4, 2.6].into_iter().map(|phase| {
            let height = 3.0 + ((position * 6.0 + phase).sin().abs() as f32) * 9.0;
            div()
                .w(px(2.))
                .h(px(height))
                .rounded_full()
                .bg(theme::accent())
        }))
}
