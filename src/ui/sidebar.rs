use gpui::{
    deferred, div, prelude::*, px, AnimationExt, AnyElement, Context, InteractiveElement,
    IntoElement, MouseButton, ParentElement, SharedString, Styled,
};

use crate::ui::cover::cover_art;
use crate::ui::icons::{self, icon, icon_scaled};
use crate::ui::main_window::MainWindow;
use crate::ui::session::{LibraryView, Playlist, PlaylistDensity};
use crate::ui::theme;

impl MainWindow {
    pub(crate) fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.read(cx);
        let view = session.view.clone();
        let all_count = session.track_count();
        let liked_count = session.liked_count();
        let playlists = session.playlists.clone();
        let current = session.current;
        let density = session.playlist_density;
        let bars_id = session.bars_playlist().map(str::to_owned);
        let playing_here =
            |playlist: &Playlist| current.is_some_and(|id| playlist.track_ids.contains(&id));

        div()
            .relative()
            .flex()
            .flex_col()
            .w(px(220.))
            .h_full()
            .mr(px(8.))
            .rounded_md()
            .bg(theme::bg_surface())
            .child(self.sidebar_drop_canvas(cx))
            .child(div().absolute().inset_0().rounded_md().with_spring(
                "sidebar-file-tint",
                Self::smooth_spring(self.sidebar_drop_lit()),
                |layer, phase| layer.bg(theme::accent_soft().opacity(phase.0)),
            ))
            .child(self.sidebar_header(cx))
            .child(self.virtual_row(
                "view-all",
                icons::ALL,
                "All tracks",
                &count_label(all_count),
                matches!(view, LibraryView::All),
                false,
                cx,
                |this, window, cx| this.select_view(LibraryView::All, window, cx),
            ))
            .child(self.virtual_row(
                "view-liked",
                icons::HEART_FILLED,
                "Liked",
                &count_label(liked_count),
                matches!(view, LibraryView::Liked),
                false,
                cx,
                |this, window, cx| this.select_view(LibraryView::Liked, window, cx),
            ))
            .child(div().mt(px(6.)).mx(px(8.)).h(px(1.)).bg(theme::border()))
            .child(self.playlist_list(playlists, view, density, playing_here, bars_id, cx))
    }

    fn sidebar_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .pt(px(8.))
            .pb(px(2.))
            .px(px(6.))
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .line_height(px(0.))
                    .text_color(theme::text_secondary())
                    .pl(px(6.))
                    .child("Library"),
            )
            .child(
                div()
                    .id("new-playlist")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded_md()
                    .cursor_pointer()
                    .child(icon(icons::PLUS, theme::text_secondary(), px(10.)))
                    .on_hover(Self::hover_listener(SharedString::from("new-playlist"), cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseDownEvent, _, _cx| {
                            this.arm("new-playlist", false);
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                            if !this.armed("new-playlist") {
                                return;
                            }
                            this.session.update(cx, |session, cx| {
                                session.create_playlist();
                                cx.notify();
                            });
                        }),
                    )
                    .with_spring(
                        "new-playlist-hover",
                        Self::smooth_spring(self.hovered_ui.as_deref() == Some("new-playlist")),
                        |button, phase| button.bg(theme::accent_soft().opacity(phase.0)),
                    ),
            )
    }

    fn virtual_row(
        &self,
        id: &'static str,
        glyph: &'static [u8],
        name: &'static str,
        count: &str,
        active: bool,
        playing: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut MainWindow, &mut gpui::Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let name_color = if playing {
            theme::accent()
        } else {
            theme::text()
        };
        let hover_id = SharedString::from(id);
        let hovered = self.hovered_ui.as_ref() == Some(&hover_id);
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(10.))
            .mx(px(6.))
            .mt(px(4.))
            .px(px(6.))
            .h(px(40.))
            .rounded_md()
            .cursor_pointer()
            .when(active, |row| row.bg(theme::accent_soft()))
            .on_hover(Self::hover_listener(hover_id.clone(), cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                    this.arm(id, false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, window, cx| {
                    if this.armed(id) {
                        action(this, window, cx);
                    }
                }),
            )
            .child(self.icon_mark(glyph, playing, 28.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(14.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(name_color)
                            .child(name),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .line_height(px(12.))
                            .text_color(theme::text_muted())
                            .child(count.to_string()),
                    ),
            )
            .with_spring(
                SharedString::from(format!("{id}-hover")),
                Self::smooth_spring(hovered),
                move |row, phase| {
                    if active {
                        row
                    } else {
                        row.bg(theme::hover().opacity(phase.0))
                    }
                },
            )
            .into_any_element()
    }

    fn icon_mark(&self, glyph: &'static [u8], playing: bool, size: f32) -> AnyElement {
        div()
            .size(px(size))
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .bg(theme::bg_elevated())
            .child(icon(
                glyph,
                if playing {
                    theme::accent()
                } else {
                    theme::text_secondary()
                },
                px(size * 0.55),
            ))
            .into_any_element()
    }

    fn playlist_mark(
        &self,
        id: String,
        cover: Option<std::path::PathBuf>,
        can_play: bool,
        size: f32,
        bars: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover_id = SharedString::from(format!("playlist-cover-{id}"));
        let hovered = can_play
            && self.item_drag.is_none()
            && !self.sidebar_drop_lit()
            && self.hovered_cover.as_ref() == Some(&hover_id);
        let bars_key = SharedString::from(format!("playlist-eq-{id}"));
        let play_id = SharedString::from(format!("playlist-play-{id}"));
        let play_size = size * 0.42;
        let fade_id = SharedString::from(format!("playlist-cover-fade-{id}"));
        div()
            .id(hover_id.clone())
            .relative()
            .size(px(size))
            .flex_shrink_0()
            .on_hover(Self::hover_cover_listener(hover_id, cx))
            .child(cover_art(cover.as_deref(), size))
            .child({
                let play_id = play_id.clone();
                let playlist_id = id.clone();
                div()
                    .id(play_id.clone())
                    .absolute()
                    .inset_0()
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(hovered, |overlay| {
                        let play_id = play_id.clone();
                        let playlist_id = playlist_id.clone();
                        overlay
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let play_id = play_id.clone();
                                    move |this, _: &gpui::MouseDownEvent, _, cx| {
                                        cx.stop_propagation();
                                        this.arm(play_id.clone(), false);
                                    }
                                }),
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                                    cx.stop_propagation();
                                    if !this.armed(&play_id) {
                                        return;
                                    }
                                    this.session.update(cx, |session, cx| {
                                        session.play_playlist(&playlist_id);
                                        cx.notify();
                                    });
                                }),
                            )
                    })
                    .with_spring(
                        fade_id,
                        Self::smooth_spring(hovered),
                        move |overlay, phase| {
                            let t = phase.0.clamp(0.0, 1.0);
                            overlay.bg(gpui::rgba(0x000000b0).opacity(t)).child(
                                div()
                                    .size(px(play_size))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon_scaled(
                                        icons::PLAY,
                                        theme::white().opacity(t),
                                        px(play_size),
                                        0.92 + 0.08 * t,
                                    )),
                            )
                        },
                    )
            })
            // The playing playlist's bars live on the cover, not beside the name.
            .when(bars && !hovered, |mark| {
                mark.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(gpui::rgba(0x00000088))
                        .child(crate::ui::track_list::eq_bars(bars_key, cx.reduce_motion())),
                )
            })
            .into_any_element()
    }

    fn playlist_list(
        &self,
        playlists: Vec<Playlist>,
        view: LibraryView,
        density: PlaylistDensity,
        playing_here: impl Fn(&Playlist) -> bool,
        bars_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let can_reorder = playlists.len() > 1;
        let renaming = self.renaming_id().map(str::to_owned);
        let slide_from = self
            .item_drag
            .as_ref()
            .and_then(|drag| drag.playlist_id())
            .and_then(|id| playlists.iter().position(|playlist| playlist.id == id));
        let slide_count = playlists.len();
        if playlists.is_empty() {
            return div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .flex_1()
                .px(px(16.))
                .gap(px(6.))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme::text())
                        .child("No playlists yet"),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .text_center()
                        .child("Create a playlist or drop a folder"),
                )
                .into_any_element();
        }

        div()
            .flex_1()
            .min_h(px(0.))
            .relative()
            .child(self.playlist_list_canvas(cx))
            .child(
                div()
                    .id("playlist-list")
                    .flex()
                    .flex_col()
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    .on_scroll_wheel(cx.listener(
                        |this, event: &gpui::ScrollWheelEvent, window, cx| {
                            this.note_scroll(
                                crate::ui::main_window::Scroller::Sidebar,
                                event,
                                window,
                                cx,
                            );
                        },
                    ))
                    .pt(px(4.))
                    .pb(px(8.))
                    .children(playlists.into_iter().enumerate().map(|(index, playlist)| {
                        let active =
                            matches!(&view, LibraryView::Playlist(id) if id == &playlist.id);
                        let playing = playing_here(&playlist);
                        let count = playlist.track_ids.len();
                        let id = playlist.id.clone();
                        let drop_name = playlist.name.clone();
                        let element_id = SharedString::from(format!("playlist-{}", playlist.id));
                        let hovered = self.hovered_ui.as_ref() == Some(&element_id);
                        let drop_here = self.drop_row_lit(&playlist.id);
                        let can_drag =
                            can_reorder && renaming.as_deref() != Some(playlist.id.as_str());
                        let editing = renaming.as_deref() == Some(playlist.id.as_str());
                        let show_bars = bars_id.as_deref() == Some(playlist.id.as_str());
                        let lifted = self
                            .item_drag
                            .as_ref()
                            .is_some_and(|drag| drag.lifts_playlist(&playlist.id));
                        let scroll = self.sidebar_scroll.offset().y.as_f32();
                        let held = lifted.then(|| {
                            self.item_drag
                                .as_ref()
                                .map(|drag| drag.hold_y(-scroll))
                                .unwrap_or(0.)
                        });
                        let delta = held.unwrap_or_else(|| {
                            slide_from
                                .and_then(|from| {
                                    self.item_drag
                                        .as_ref()
                                        .map(|drag| drag.playlist_slide(index, slide_count, from))
                                })
                                .unwrap_or(0.)
                        });
                        let slot_h = playlist_slot_h(density);
                        let name_color = if playing {
                            theme::accent()
                        } else {
                            theme::text()
                        };
                        let row = div()
                            .id(element_id.clone())
                            .relative()
                            .child(div().absolute().inset_0().rounded_md().with_spring(
                                SharedString::from(format!("playlist-drop-{}", playlist.id)),
                                Self::smooth_spring(drop_here),
                                |layer, phase| layer.bg(theme::accent_soft().opacity(phase.0)),
                            ))
                            .flex()
                            .flex_shrink(0.)
                            .items_center()
                            .mx(px(6.))
                            .px(px(6.))
                            .rounded_md()
                            .cursor_pointer()
                            .when(density == PlaylistDensity::Normal, |row| {
                                row.gap(px(10.)).py(px(6.))
                            })
                            .when(density == PlaylistDensity::Compact, |row| {
                                row.gap(px(10.))
                                    .h(px(40.))
                                    .min_h(px(40.))
                                    .max_h(px(40.))
                                    .overflow_hidden()
                            })
                            .when(density == PlaylistDensity::Nano, |row| {
                                row.h(px(28.))
                                    .min_h(px(28.))
                                    .max_h(px(28.))
                                    .overflow_hidden()
                                    .px(px(8.))
                            })
                            .when(active, |row| row.bg(theme::accent_soft()))
                            .on_hover(Self::hover_listener(element_id.clone(), cx))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener({
                                    let element_id = element_id.clone();
                                    move |this, event: &gpui::MouseDownEvent, _, _cx| {
                                        if can_drag {
                                            this.press_at = Some(event.position);
                                        }
                                        this.arm(element_id.clone(), false);
                                    }
                                }),
                            )
                            .when(can_drag, {
                                let drag_id = id.clone();
                                move |row| {
                                    row.on_drag(
                                        crate::ui::item_drag::PlaylistDrag { id: drag_id },
                                        crate::ui::item_drag::spawn_playlist_ghost(),
                                    )
                                }
                            })
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener({
                                    let element_id = element_id.clone();
                                    move |this, _: &gpui::MouseUpEvent, window, cx| {
                                        if !this.armed(&element_id) {
                                            return;
                                        }
                                        this.select_view(
                                            LibraryView::Playlist(id.clone()),
                                            window,
                                            cx,
                                        );
                                    }
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener({
                                    let id = playlist.id.clone();
                                    move |this, event: &gpui::MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        this.open_playlist_menu(
                                            id.clone(),
                                            event.position,
                                            window,
                                            cx,
                                        );
                                    }
                                }),
                            )
                            .when(density == PlaylistDensity::Normal, |row| {
                                row.child(self.playlist_mark(
                                    playlist.id.clone(),
                                    playlist.cover.clone(),
                                    count > 0,
                                    38.,
                                    show_bars && !editing,
                                    cx,
                                ))
                            })
                            .when(density == PlaylistDensity::Compact, |row| {
                                row.child(self.playlist_mark(
                                    playlist.id.clone(),
                                    playlist.cover.clone(),
                                    count > 0,
                                    28.,
                                    show_bars && !editing,
                                    cx,
                                ))
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .gap(px(if density == PlaylistDensity::Compact {
                                        0.
                                    } else {
                                        2.
                                    }))
                                    .child(if editing {
                                        div()
                                            .flex()
                                            .w_full()
                                            .h(px(20.))
                                            .px(px(4.))
                                            .rounded(px(3.))
                                            .bg(theme::bg_elevated())
                                            .border_1()
                                            .border_color(theme::accent())
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                |_: &gpui::MouseDownEvent, _, cx| {
                                                    cx.stop_propagation();
                                                },
                                            )
                                            .on_mouse_up(
                                                MouseButton::Left,
                                                |_: &gpui::MouseUpEvent, _, cx| {
                                                    cx.stop_propagation();
                                                },
                                            )
                                            .child(self.rename_field.clone())
                                            .into_any_element()
                                    } else {
                                        div()
                                            .text_size(px(12.))
                                            .when(density != PlaylistDensity::Nano, |name| {
                                                name.line_height(px(14.))
                                            })
                                            .font_weight(if density == PlaylistDensity::Normal {
                                                gpui::FontWeight::MEDIUM
                                            } else {
                                                gpui::FontWeight::SEMIBOLD
                                            })
                                            .text_color(name_color)
                                            .truncate()
                                            .child(playlist.name)
                                            .into_any_element()
                                    })
                                    .when(density != PlaylistDensity::Nano, |col| {
                                        col.child(
                                            div()
                                                .text_size(px(10.))
                                                .when(density != PlaylistDensity::Nano, |count| {
                                                    count.line_height(px(12.))
                                                })
                                                .text_color(theme::text_muted())
                                                .child(count_label(count)),
                                        )
                                    }),
                            )
                            .when(
                                show_bars && !editing && density == PlaylistDensity::Nano,
                                |row| {
                                    // Nano has no cover, so the bars keep their spot by the name.
                                    let bars_key =
                                        SharedString::from(format!("playlist-eq-{}", playlist.id));
                                    row.child(div().ml(px(8.)).flex_shrink_0().child(
                                        crate::ui::track_list::eq_bars(
                                            bars_key,
                                            cx.reduce_motion(),
                                        ),
                                    ))
                                },
                            )
                            .with_spring(
                                SharedString::from(format!("{element_id}-hover")),
                                Self::smooth_spring(hovered && !drop_here),
                                move |row, phase| {
                                    if active {
                                        row
                                    } else {
                                        row.bg(theme::hover().opacity(phase.0))
                                    }
                                },
                            );
                        // The row is out of flow. Without a frozen slot height the
                        // flex column shrinks every row to fit, so the list never
                        // overflows and the wheel has nothing to scroll.
                        // Neighbors ease into the hole. The dragged row stays put:
                        // its preview is pinned to the pointer, not sprung.
                        let slide_id = SharedString::from(format!("{element_id}-slide"));
                        div()
                            .relative()
                            .w_full()
                            .h(px(slot_h))
                            .min_h(px(slot_h))
                            .max_h(px(slot_h))
                            .flex_shrink_0()
                            .child(
                                div()
                                    .absolute()
                                    .top(px(0.))
                                    .bottom(px(0.))
                                    .left(px(6.))
                                    .right(px(6.))
                                    .child(self.playlist_drop_canvas(
                                        playlist.id.clone(),
                                        drop_name,
                                        cx,
                                    )),
                            )
                            .child(if held.is_some() {
                                // The held row keeps its look and rides the pointer. Deferred
                                // so it paints above the rows it passes instead of under them.
                                deferred(
                                    div()
                                        .absolute()
                                        .left(px(0.))
                                        .right(px(0.))
                                        .top(px(delta))
                                        .flex()
                                        .flex_col()
                                        .child(row),
                                )
                                .with_priority(1)
                                .into_any_element()
                            } else {
                                div()
                                    .absolute()
                                    .left(px(0.))
                                    .right(px(0.))
                                    .flex()
                                    .flex_col()
                                    .with_spring(
                                        slide_id,
                                        Self::smooth_spring_px(delta),
                                        |row, top| row.top(top),
                                    )
                                    .child(row)
                                    .into_any_element()
                            })
                    })),
            )
            .child(self.scrollbar(crate::ui::main_window::Scroller::Sidebar, cx))
            .into_any_element()
    }

    pub(crate) fn select_view(
        &mut self,
        view: LibraryView,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        self.session.update(cx, |session, cx| {
            session.view = view;
            cx.notify();
        });
        self.focus.focus(window, cx);
    }
}

/// Layout height of a playlist row. The sliding visual is taken out of flow,
/// so the slot has to keep this height or the list collapses.
fn playlist_slot_h(density: PlaylistDensity) -> f32 {
    match density {
        // py 6 + cover 38.
        PlaylistDensity::Normal => 50.,
        PlaylistDensity::Compact => 40.,
        PlaylistDensity::Nano => 28.,
    }
}

fn count_label(count: usize) -> String {
    if count == 1 {
        "1 track".into()
    } else {
        format!("{count} tracks")
    }
}
