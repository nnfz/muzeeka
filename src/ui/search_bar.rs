use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, uniform_list, AnimationExt, AnyElement, Context, CursorStyle,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, SpringAnimation,
    SpringConfig, Styled, Window,
};

use crate::ui::cover::cover_art;
use crate::ui::icons::{self, icon};
use crate::ui::main_window::{MainWindow, Scroller};
use crate::ui::session::{self, has_search_text, looks_like_url, Suggestion, TrackId};
use crate::ui::theme;

/// Search hit row. Cover is 28px, with 6px of air above and below.
const SEARCH_ROW_H: f32 = 40.;
/// Cap for the results scroller. The plate stays this tall once the hits overflow.
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
        let url = looks_like_url(&query);

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
            .when(url, |row| row.child(self.download_chip(cx)))
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
                cx.listener(|this, _: &gpui::MouseUpEvent, window, cx| {
                    if !this.armed("search-clear") {
                        return;
                    }
                    this.search.update(cx, |field, cx| field.clear(window, cx));
                }),
            )
            .into_any_element()
    }

    fn download_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("search-download")
            .px(px(8.))
            .h(px(22.))
            .flex()
            .items_center()
            .w_full()
            .rounded_sm()
            .bg(theme::accent_soft())
            .text_size(px(11.))
            .text_color(theme::accent())
            .cursor_pointer()
            .child("Download")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.arm("search-download", false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if !this.armed("search-download") {
                        return;
                    }
                    this.session.update(cx, |session, cx| {
                        session.notice =
                            Some("Downloads are not connected in the native port yet.".into());
                        cx.notify();
                    });
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
        let (suggestions, notice) = {
            let session = self.session.read(cx);
            (session.suggestions(&query), session.notice.clone())
        };
        let show_suggestions = focused && !suggestions.is_empty() && !looks_like_url(&query);
        // One plate at a time. `@` opens the filter menu, not the results list.
        let show_results = has_search_text(&query) && suggestions.is_empty();
        let expanded = focused || !query.is_empty();
        let hits = show_results.then(|| self.cached_search_ids(&query, cx));
        if hits.as_ref().is_some_and(|hits| !hits.is_empty()) {
            self.warm_search_scrollbar(window, cx);
        }
        let described = session::describe_search(&query);

        div()
            .absolute()
            .top(px(46.))
            .left(px(8.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .when(show_suggestions || notice.is_some(), |layer| {
                layer.child(fade_in(
                    "search-suggest-in",
                    self.suggestion_plate(suggestions, show_suggestions, notice, cx),
                ))
            })
            .when_some(hits, |layer, hits| {
                layer.child(fade_in(
                    "search-results-in",
                    self.results_plate(hits, described, cx),
                ))
            })
            .with_spring(
                "search-suggestions-width",
                search_width_spring(expanded),
                |layer, width| layer.w(width),
            )
    }

    fn suggestion_plate(
        &self,
        suggestions: Vec<Suggestion>,
        show: bool,
        notice: Option<String>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .occlude()
            .flex()
            .flex_col()
            .rounded_md()
            .bg(theme::bg_elevated())
            .border_1()
            .border_color(theme::border())
            .overflow_hidden()
            .when_some(notice, |layer, notice| {
                layer.child(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .text_size(px(12.))
                        .text_color(theme::text_secondary())
                        .child(notice),
                )
            })
            .when(show, |layer| {
                layer.children(
                    suggestions
                        .into_iter()
                        .enumerate()
                        .map(|(index, suggestion)| {
                            let insert = suggestion.insert.clone();
                            let suggestion_id =
                                gpui::SharedString::from(format!("suggestion-{index}"));
                            let arm_id = suggestion_id.clone();
                            let release_id = suggestion_id.clone();
                            div()
                                .id(suggestion_id)
                                .flex()
                                .items_center()
                                .justify_between()
                                .h(px(36.))
                                .px(px(12.))
                                .cursor_pointer()
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
                                        .text_size(px(11.))
                                        .text_color(theme::text_muted())
                                        .child(suggestion.detail),
                                )
                        }),
                )
            })
    }

    fn results_plate(
        &self,
        hits: Arc<Vec<TrackId>>,
        description: String,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let count = hits.len();
        let title = if count == 1 {
            "1 result".to_string()
        } else {
            format!("{count} results")
        };
        div()
            .occlude()
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
                self.result_rows(hits, cx).into_any_element()
            })
    }

    fn result_rows(&self, hits: Arc<Vec<TrackId>>, cx: &mut Context<Self>) -> impl IntoElement {
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
                        // One element per index. A short vec would slide the
                        // following rows up while the scroll height stayed put.
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
                                    ))
                                })
                                .collect::<Vec<_>>()
                        };
                        prepared
                            .into_iter()
                            .map(|hit| match hit {
                                Some((id, title, artist, cover, duration, playlist, current)) => {
                                    view.search_result_row(
                                        id,
                                        title,
                                        artist,
                                        cover,
                                        duration,
                                        playlist,
                                        current,
                                        Arc::clone(&hits),
                                        cx,
                                    )
                                }
                                None => div().h(px(SEARCH_ROW_H)).into_any_element(),
                            })
                            .collect()
                    })
                })
                .h(px(list_h))
                .w_full()
                .track_scroll(&self.search_scroll)
                .on_scroll_wheel(cx.listener(
                    |this, event: &gpui::ScrollWheelEvent, window, cx| {
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
        hits: Arc<Vec<TrackId>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = current == Some(id);
        let row_id = gpui::SharedString::from(format!("search-hit-{}", id.0));
        let arm_id = row_id.clone();
        let release_id = row_id.clone();
        let secondary = match playlist {
            Some(name) => format!("{artist} · {name}"),
            None => artist,
        };
        let features = gpui::FontFeatures(Arc::new(vec![("tnum".into(), 1)]));
        div()
            .id(row_id)
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(SEARCH_ROW_H))
            .px(px(12.))
            .cursor_pointer()
            .when(active, |row| row.bg(theme::accent_soft()))
            .hover(|style| {
                style.bg(if active {
                    theme::accent_soft()
                } else {
                    theme::hover()
                })
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.arm(arm_id.clone(), false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if !this.armed(&release_id) {
                        return;
                    }
                    let queue = hits.as_ref().clone();
                    this.session.update(cx, |session, cx| {
                        session.play(id, queue);
                        cx.notify();
                    });
                }),
            )
            .child(cover_art(cover.as_deref(), 28.))
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
            .into_any_element()
    }
}

/// Collapsed 220px, expanded 480px. Critically damped so the bar eases and
/// does not overshoot into the window buttons. About a quarter second.
fn search_width_spring(expanded: bool) -> SpringAnimation<Pixels> {
    let width = if expanded { px(480.) } else { px(220.) };
    let stiffness = 420.;
    SpringAnimation::new(SpringConfig::new(stiffness, 2. * stiffness.sqrt(), 1.)).to(width)
}

/// Opacity 0→1 and a 6px drop, matching the web search dropdown.
/// The plate mounts only while it is shown, so `.from(0.)` replays each time.
fn search_enter_spring() -> SpringAnimation<f32> {
    let stiffness = 420.;
    SpringAnimation::new(SpringConfig::new(stiffness, 2. * stiffness.sqrt(), 1.))
        .to(1.)
        .from(0.)
}

fn fade_in(id: &'static str, plate: gpui::Div) -> impl IntoElement {
    plate.with_spring(id, search_enter_spring(), |plate, phase| {
        let phase = phase.clamp(0.0, 1.0);
        // Negative top is translateY(-6px): the plate starts above its slot and slides down.
        plate.opacity(phase).top(px(-6. * (1.0 - phase)))
    })
}
