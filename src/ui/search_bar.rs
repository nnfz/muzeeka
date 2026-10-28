use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, uniform_list, AnimationExt, AnyElement, Context, CursorStyle,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, SpringAnimation,
    SpringConfig, Styled, Window,
};

use crate::ui::icons::{self, icon};
use crate::ui::main_window::{MainWindow, Scroller};
use crate::ui::session::{self, has_search_text, looks_like_url, Suggestion, TrackId};
use crate::ui::theme;
use crate::ui::ytdlp;

const SEARCH_ROW_H: f32 = 40.;
const SEARCH_LIST_MAX_H: f32 = 328.;

impl MainWindow {
    pub(crate) fn search_bar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self.search.read(cx).content.clone();
        let focused = self.search.read(cx).focus.is_focused(window);
        let expanded = focused || !query.is_empty();

        div()
            .id("search-shell")
            .occlude()
            .flex()
            .items_center()
            .h(px(32.))
            .px(px(8.))
            .gap(px(6.))
            .rounded_md()
            .bg(theme::bg_glass())
            .cursor(CursorStyle::IBeam)
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, true, window.mouse_position(), cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    let focus = this.search.read(cx).focus.clone();
                    focus.focus(window, cx);
                }),
            )
            .child(icon(icons::SEARCH, theme::text_muted(), px(18.)))
            .child(self.search.clone())
            .when(!query.is_empty(), |row| {
                row.child(self.clear_search_button(cx))
            })
            .with_spring(
                "search-width",
                search_width_spring(expanded),
                |bar, width| bar.w(width),
            )
    }

    fn clear_search_button(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("search-clear")
            .flex()
            .items_center()
            .justify_center()
            .size(px(18.))
            .rounded_sm()
            .cursor_pointer()
            .text_size(px(14.))
            .text_color(theme::text_muted())
            .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
            .child("×")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.arm("search-clear", false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _window, cx| {
                    if !this.armed("search-clear") {
                        return;
                    }
                    this.search.update(cx, |field, cx| field.clear(cx));
                    this.note_search_cleared();
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    pub(crate) fn search_suggestions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let query = self.search.read(cx).content.clone();
        let focused = self.search.read(cx).focus.is_focused(window);
        let suggestions = self.session.read(cx).suggestions(&query);
        let media = ytdlp::is_media_url(&query);
        let show_suggestions =
            focused && !suggestions.is_empty() && !looks_like_url(&query) && !media;
        let show_results = has_search_text(&query) && suggestions.is_empty() && !media;
        let expanded = focused || !query.is_empty();
        let hits = show_results.then(|| self.cached_search_ids(&query, cx));
        if hits.as_ref().is_some_and(|hits| !hits.is_empty()) {
            self.warm_search_scrollbar(window, cx);
        }
        let described = session::describe_search(&query);
        let suggestion_pick = self.picked_index(&query, suggestions.len());
        let result_pick = hits
            .as_ref()
            .map(|hits| self.picked_index(&query, hits.len()))
            .unwrap_or(0);

        div()
            .absolute()
            .top(px(46.))
            .left(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .when(show_suggestions, |layer| {
                layer.child(fade_in(
                    "search-suggest-in",
                    self.suggestion_plate(suggestions, suggestion_pick, cx),
                ))
            })
            .when_some(hits, |layer, hits| {
                layer.child(fade_in(
                    "search-results-in",
                    self.results_plate(hits, described, result_pick, cx),
                ))
            })
            .with_spring(
                "search-suggestions-width",
                search_width_spring(expanded),
                |layer, width| layer.w(width),
            )
    }

    pub(crate) fn accept_search_suggestion(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if ytdlp::is_media_url(query) {
            return false;
        }
        let len = self.search_choice_count(query, cx);
        let index = self.picked_index(query, len);
        let choice = session::search_tab(self.session.read(cx), query, index);
        match choice {
            Some(session::SearchTab::Complete(insert)) => {
                let search = self.search.clone();
                window.defer(cx, move |window, cx| {
                    search.update(cx, |field, cx| field.set_text(insert, window, cx));
                });
                true
            }
            Some(session::SearchTab::Reveal(id)) => {
                let entity = cx.entity().clone();
                window.defer(cx, move |window, cx| {
                    entity.update(cx, |this, cx| this.go_to_search_track(id, window, cx));
                });
                true
            }
            None => false,
        }
    }

    pub(crate) fn move_search_pick(
        &mut self,
        query: &str,
        delta: i32,
        cx: &mut Context<Self>,
    ) -> bool {
        let len = self.search_choice_count(query, cx);
        let Some(next) = session::step_pick(len, self.picked_index(query, len), delta) else {
            return false;
        };
        let showing_results = self.session.read(cx).suggestions(query).is_empty();
        if next != self.search_pick || self.search_pick_query != query {
            self.search_pick = next;
            self.search_pick_query = query.to_string();
            if showing_results {
                self.search_scroll
                    .scroll_to_item(next, gpui::ScrollStrategy::Nearest);
            }
            cx.notify();
        }
        true
    }

    fn search_choice_count(&self, query: &str, cx: &Context<Self>) -> usize {
        if ytdlp::is_media_url(query) || looks_like_url(query) {
            return 0;
        }
        let session = self.session.read(cx);
        let suggestions = session.suggestions(query).len();
        if suggestions > 0 {
            return suggestions;
        }
        if has_search_text(query) {
            session.search_ids(query).len()
        } else {
            0
        }
    }

    fn picked_index(&self, query: &str, len: usize) -> usize {
        if len == 0 || self.search_pick_query != query {
            return 0;
        }
        self.search_pick.min(len - 1)
    }

    fn go_to_search_track(&mut self, id: TrackId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spot) = self.session.read(cx).spot_for_track(id) else {
            return;
        };
        let view = spot.view.clone();
        let track_id = spot.track_id;
        self.search.update(cx, |field, cx| field.clear(cx));
        self.note_search_cleared();
        self.reveal_in_playlist(track_id, view, window, cx);
    }

    fn suggestion_plate(
        &self,
        suggestions: Vec<Suggestion>,
        pick: usize,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("search-suggest-plate")
            .occlude()
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, true, window.mouse_position(), cx);
                }),
            )
            .flex()
            .flex_col()
            .rounded_md()
            .bg(theme::bg_elevated())
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .children(
                suggestions
                    .into_iter()
                    .enumerate()
                    .map(|(index, suggestion)| {
                        let insert = suggestion.insert.clone();
                        let suggestion_id = gpui::SharedString::from(format!("suggestion-{index}"));
                        let arm_id = suggestion_id.clone();
                        let release_id = suggestion_id.clone();
                        div()
                            .id(suggestion_id)
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_between()
                            .h(px(36.))
                            .px(px(12.))
                            .cursor_pointer()
                            .when(index == pick, |row| row.bg(theme::accent_soft()))
                            .hover(|style| style.bg(theme::accent_soft()))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                                    this.arm(arm_id.clone(), false);
                                }),
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _: &gpui::MouseUpEvent, window, cx| {
                                    if !this.armed(&release_id) {
                                        return;
                                    }
                                    let insert = insert.clone();
                                    this.search.update(cx, |field, cx| {
                                        field.set_text(insert, window, cx);
                                    });
                                    let focus = this.search.read(cx).focus.clone();
                                    focus.focus(window, cx);
                                }),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme::text())
                                    .child(suggestion.label),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .when(index == pick, |side| {
                                        side.child(
                                            div()
                                                .px(px(4.))
                                                .rounded_sm()
                                                .border_1()
                                                .border_color(theme::border())
                                                .text_size(px(10.))
                                                .text_color(theme::text_muted())
                                                .child("Tab"),
                                        )
                                    })
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(theme::text_muted())
                                            .child(suggestion.detail),
                                    ),
                            )
                    }),
            )
    }

    fn results_plate(
        &self,
        hits: Arc<Vec<TrackId>>,
        description: String,
        pick: usize,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let count = hits.len();
        let title = if count == 1 {
            "1 result".to_string()
        } else {
            format!("{count} results")
        };
        div()
            .id("search-results-plate")
            .w_full()
            .occlude()
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, true, window.mouse_position(), cx);
                }),
            )
            .flex()
            .flex_col()
            .rounded_md()
            .bg(theme::bg_elevated())
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .gap(px(10.))
                    .h(px(32.))
                    .px(px(12.))
                    .bg(theme::bg_surface())
                    .border_b_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                            .whitespace_nowrap()
                            .child(title),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(10.))
                            .text_color(theme::text_muted())
                            .truncate()
                            .child(description),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(10.))
                            .text_color(theme::text_muted())
                            .whitespace_nowrap()
                            .child("@a · @t · @p · @artist · @title · @playlist"),
                    ),
            )
            .child(if hits.is_empty() {
                div()
                    .px(px(12.))
                    .py(px(16.))
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .text_center()
                    .child("No matches — try @a, @t, @p, @artist, @title or @playlist")
                    .into_any_element()
            } else {
                self.result_rows(hits, pick, cx).into_any_element()
            })
    }

    fn result_rows(
        &self,
        hits: Arc<Vec<TrackId>>,
        pick: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let count = hits.len();
        let list_h = (count as f32 * SEARCH_ROW_H).min(SEARCH_LIST_MAX_H);
        let entity = cx.entity().clone();
        div()
            .relative()
            .w_full()
            .h(px(list_h))
            .child(
                uniform_list("search-results", count, move |range, _window, app| {
                    let hits = Arc::clone(&hits);
                    let entity = entity.clone();
                    entity.update(app, |view, cx| {
                        let prepared = {
                            let session = view.session.read(cx);
                            range
                                .map(|row| {
                                    let id = hits.get(row).copied()?;
                                    let track = session.track(id)?;
                                    Some((
                                        id,
                                        track.title.clone(),
                                        track.artist.clone(),
                                        track.cover.clone(),
                                        track.duration,
                                        session.playlist_label(id),
                                        session.current,
                                        row == pick,
                                    ))
                                })
                                .collect::<Vec<_>>()
                        };
                        prepared
                            .into_iter()
                            .map(|hit| match hit {
                                Some((
                                    id,
                                    title,
                                    artist,
                                    cover,
                                    duration,
                                    playlist,
                                    current,
                                    primary,
                                )) => view.search_result_row(
                                    id, title, artist, cover, duration, playlist, current, primary,
                                    cx,
                                ),
                                None => div().w_full().h(px(SEARCH_ROW_H)).into_any_element(),
                            })
                            .collect()
                    })
                })
                .h(px(list_h))
                .w_full()
                .track_scroll(&self.search_scroll)
                .on_scroll(cx.listener(
                    |this, event: &gpui::ScrollEvent, window, cx| {
                        this.note_scroll(Scroller::Search, event, window, cx);
                    },
                )),
            )
            .child(self.scrollbar(Scroller::Search, cx))
    }

    fn search_result_row(
        &self,
        id: TrackId,
        title: String,
        artist: String,
        cover: Option<PathBuf>,
        duration: f64,
        playlist: Option<String>,
        current: Option<TrackId>,
        primary: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = current == Some(id);
        let row_id = gpui::SharedString::from(format!("search-hit-{}", id.0));
        let secondary = match playlist {
            Some(name) => format!("{artist} · {name}"),
            None => artist,
        };
        let features = gpui::FontFeatures(Arc::new(vec![("tnum".into(), 1)]));
        let cover_key = format!("search-{}", id.0);
        let go_key = cover_key.clone();
        div()
            .id(row_id)
            .w_full()
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(SEARCH_ROW_H))
            .px(px(12.))
            .when(active || primary, |row| row.bg(theme::accent_soft()))
            .hover(|style| {
                style.bg(if active || primary {
                    theme::accent_soft()
                } else {
                    theme::hover()
                })
            })
            .child(
                self.cover_with_play(&cover_key, 28., cover.as_deref(), cx, move |this, cx| {
                    this.session.update(cx, |session, cx| {
                        session.play_track(id, None);
                        cx.notify();
                    });
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(1.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(14.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .truncate()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .line_height(px(12.))
                            .text_color(theme::text_secondary())
                            .truncate()
                            .child(secondary),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(10.))
                    .text_color(theme::text_muted())
                    .font_features(features)
                    .child(session::format_time(duration)),
            )
            .when(primary, |row| {
                row.child(
                    div()
                        .flex_shrink_0()
                        .px(px(4.))
                        .rounded_sm()
                        .border_1()
                        .border_color(theme::border())
                        .text_size(px(10.))
                        .text_color(theme::text_muted())
                        .child("Tab"),
                )
            })
            .child(
                self.go_to_track_button(&go_key, cx, move |this, window, cx| {
                    this.go_to_search_track(id, window, cx);
                }),
            )
            .into_any_element()
    }
}

fn search_width_spring(expanded: bool) -> SpringAnimation<Pixels> {
    let width = if expanded { px(480.) } else { px(220.) };
    let stiffness = 420.;
    SpringAnimation::new(SpringConfig::new(stiffness, 2. * stiffness.sqrt(), 1.)).to(width)
}

fn search_enter_spring() -> SpringAnimation<f32> {
    let stiffness = 420.;
    SpringAnimation::new(SpringConfig::new(stiffness, 2. * stiffness.sqrt(), 1.))
        .to(1.)
        .from(0.)
}

fn fade_in(id: &'static str, plate: gpui::Stateful<gpui::Div>) -> impl IntoElement {
    plate.with_spring(id, search_enter_spring(), |plate, phase| {
        let phase = phase.clamp(0.0, 1.0);
        plate
            .relative()
            .w_full()
            .left(px(0.))
            .opacity(phase)
            .top(px(-6. * (1.0 - phase)))
    })
}
