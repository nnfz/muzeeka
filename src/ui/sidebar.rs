use gpui::{
    div, prelude::*, px, AnimationExt, AnyElement, Context, InteractiveElement, IntoElement,
    MouseButton, ParentElement, SharedString, Styled,
};

use crate::ui::cover::cover_art;
use crate::ui::icons::{self, icon};
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
        let playing_here =
            |playlist: &Playlist| current.is_some_and(|id| playlist.track_ids.contains(&id));

        div()
            .flex()
            .flex_col()
            .w(px(220.))
            .h_full()
            .mr(px(8.))
            .rounded_md()
            .bg(theme::bg_surface())
            .child(self.sidebar_header(cx))
            .child(self.virtual_row(
                "view-all",
                icons::LIST,
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
            .child(self.playlist_list(playlists, view, density, playing_here, cx))
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
                        Self::hover_spring(self.hovered_ui.as_deref() == Some("new-playlist")),
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
                Self::hover_spring(hovered),
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

    fn playlist_mark(&self, cover: Option<std::path::PathBuf>, size: f32) -> AnyElement {
        cover_art(cover.as_deref(), size)
    }

    fn playlist_list(
        &self,
        playlists: Vec<Playlist>,
        view: LibraryView,
        density: PlaylistDensity,
        playing_here: impl Fn(&Playlist) -> bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                    .children(playlists.into_iter().map(|playlist| {
                let active = matches!(&view, LibraryView::Playlist(id) if id == &playlist.id);
                let playing = playing_here(&playlist);
                let count = playlist.track_ids.len();
                let id = playlist.id.clone();
                let element_id = SharedString::from(format!("playlist-{}", playlist.id));
                let hovered = self.hovered_ui.as_ref() == Some(&element_id);
                let name_color = if playing {
                    theme::accent()
                } else {
                    theme::text()
                };
                div()
                    .id(element_id.clone())
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
                            move |this, _: &gpui::MouseDownEvent, _, _cx| {
                                this.arm(element_id.clone(), false);
                            }
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener({
                            let element_id = element_id.clone();
                            move |this, _: &gpui::MouseUpEvent, window, cx| {
                                if !this.armed(&element_id) {
                                    return;
                                }
                                this.select_view(LibraryView::Playlist(id.clone()), window, cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener({
                            let id = playlist.id.clone();
                            move |this, event: &gpui::MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_playlist_menu(id.clone(), event.position, window, cx);
                            }
                        }),
                    )
                    .when(density == PlaylistDensity::Normal, |row| {
                        row.child(self.playlist_mark(playlist.cover.clone(), 38.))
                    })
                    .when(density == PlaylistDensity::Compact, |row| {
                        row.child(self.playlist_mark(playlist.cover.clone(), 28.))
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
                            .child(
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
                                    .child(playlist.name),
                            )
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
                    .with_spring(
                        element_id,
                        Self::hover_spring(hovered),
                        move |row, phase| {
                            if active {
                                row
                            } else {
                                row.bg(theme::hover().opacity(phase.0))
                            }
                        },
                    )
            }))
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

fn count_label(count: usize) -> String {
    if count == 1 {
        "1 track".into()
    } else {
        format!("{count} tracks")
    }
}
