use gpui::{
    canvas, div, prelude::*, px, relative, AnimationExt, AnyElement, Bounds, Context,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, SharedString, Styled,
};

use crate::ui::cover::cover_art;
use crate::ui::icons::{self, icon_scaled, scaled_disc};
use crate::ui::main_window::{MainWindow, SliderDrag};
use crate::ui::session::{self, RepeatMode, TrackId};
use crate::ui::theme;

const TRANSPORT_LABEL_CHARS: usize = 40;

fn limit_label(text: &str) -> String {
    if text.chars().count() <= TRANSPORT_LABEL_CHARS {
        return text.to_string();
    }
    let mut shortened: String = text.chars().take(TRANSPORT_LABEL_CHARS - 1).collect();
    shortened.truncate(shortened.trim_end().len());
    shortened.push('…');
    shortened
}

impl MainWindow {
    pub(crate) fn transport(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let (
            title,
            artist,
            time,
            duration,
            progress,
            playing,
            has_track,
            shuffle,
            repeat,
            volume,
            cover,
            current_like,
        ) = {
            let session = self.session.read(cx);
            let title = session
                .current_track()
                .map(|track| limit_label(&track.title))
                .unwrap_or_else(|| "Nothing playing".into());
            let artist = session
                .current_track()
                .map(|track| limit_label(&track.artist))
                .unwrap_or_default();
            let time = session::format_time(session.position);
            let duration = session
                .current_track()
                .map(|track| session::format_time(track.duration))
                .unwrap_or_else(|| "0:00".into());
            (
                title,
                artist,
                time,
                duration,
                session.progress(),
                session.playing,
                session.current.is_some(),
                session.shuffle,
                session.repeat,
                session.volume,
                session
                    .current_track()
                    .and_then(|track| track.cover.clone()),
                session.current_track().map(|track| (track.id, track.liked)),
            )
        };

        div()
            .absolute()
            .left(px(8.))
            .right(px(8.))
            .bottom(px(8.))
            .h(px(86.))
            .flex()
            .flex_col()
            .rounded_md()
            .bg(theme::bg_surface())
            .px(px(8.))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .child(self.now_playing(&title, &artist, cover, current_like, cx))
                    .child(self.transport_controls(playing, has_track, shuffle, repeat, cx))
                    .child(self.volume_control(volume, cx)),
            )
            .child(self.progress_bar(progress, &time, &duration, cx))
    }

    fn now_playing(
        &self,
        title: &str,
        artist: &str,
        cover: Option<std::path::PathBuf>,
        current_like: Option<(TrackId, bool)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .items_center()
            .gap(px(10.))
            .child(cover_art(cover.as_deref(), 48.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .flex_1()
                    .min_w(px(0.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_shrink(1.)
                            .min_w(px(0.))
                            .overflow_hidden()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(16.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme::text())
                                    .truncate()
                                    .child(title.to_string()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .line_height(px(16.))
                                    .text_color(theme::text_muted())
                                    .truncate()
                                    .child(artist.to_string()),
                            ),
                    )
                    .when_some(current_like, |row, (track_id, liked)| {
                        row.child(self.like_button(
                            SharedString::from(format!("transport-like-{}", track_id.0)),
                            liked,
                            track_id,
                            58.,
                            16.,
                            theme::text(),
                            false,
                            cx,
                        ))
                    }),
            )
            .into_any_element()
    }

    fn transport_controls(
        &self,
        playing: bool,
        has_track: bool,
        shuffle: bool,
        repeat: RepeatMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mode = theme::text_secondary();
        div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(12.))
            .child(self.mode_button(
                "shuffle",
                if shuffle {
                    icons::SHUFFLE
                } else {
                    icons::SHUFFLE_OFF
                },
                shuffle,
                cx,
                |this, _, cx| {
                    this.session.update(cx, |session, cx| {
                        session.toggle_shuffle();
                        cx.notify();
                    });
                },
            ))
            .child(self.glyph_button(
                "prev",
                icons::PREV,
                None,
                34.,
                20.,
                mode,
                cx,
                |this, _, cx| {
                    this.session.update(cx, |session, cx| {
                        session.prev();
                        cx.notify();
                    });
                },
            ))
            .child(self.glyph_button(
                "play",
                icons::PLAY,
                Some((icons::PAUSE, playing)),
                44.,
                30.,
                theme::text(),
                cx,
                move |this, _, cx| {
                    let queue = this
                        .session
                        .read(cx)
                        .visible_tracks("")
                        .into_iter()
                        .map(|track| track.id)
                        .collect();
                    this.session.update(cx, |session, cx| {
                        if has_track {
                            session.toggle_playback(queue);
                        } else {
                            session.toggle_playback(queue);
                        }
                        cx.notify();
                    });
                },
            ))
            .child(self.glyph_button(
                "next",
                icons::NEXT,
                None,
                34.,
                20.,
                mode,
                cx,
                |this, _, cx| {
                    this.session.update(cx, |session, cx| {
                        session.next();
                        cx.notify();
                    });
                },
            ))
            .child(self.mode_button(
                "repeat",
                match repeat {
                    RepeatMode::Off => icons::REPEAT_OFF,
                    RepeatMode::All => icons::REPEAT_ALL,
                    RepeatMode::One => icons::REPEAT_ONE,
                },
                repeat != RepeatMode::Off,
                cx,
                |this, _, cx| {
                    this.session.update(cx, |session, cx| {
                        session.cycle_repeat();
                        cx.notify();
                    });
                },
            ))
    }

    fn glyph_button(
        &self,
        id: &'static str,
        glyph: &'static [u8],
        // Second glyph crossfades in when the bool is true. Both stay mounted
        // so the play/pause swap does not drop the icon for a frame.
        swap: Option<(&'static [u8], bool)>,
        size: f32,
        icon_size: f32,
        color: impl Into<gpui::Hsla> + Copy,
        cx: &mut Context<Self>,
        action: impl Fn(&mut MainWindow, &mut gpui::Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let color = color.into();
        let hovered = self.hovered_ui.as_deref() == Some(id);
        let pressed = self.pressed_ui.as_deref() == Some(id);
        let hover_id = SharedString::from(format!("{id}-hover"));
        let on_hover = Self::hover_listener(SharedString::from(id), cx);
        // Shrink starts on press. The action waits for release on this button.
        // Mouse up, not on_click: a pending click makes GPUI drop the hover.
        let on_press = cx.listener(
            move |this: &mut MainWindow, _: &gpui::MouseDownEvent, _, cx| {
                this.arm(id, true);
                cx.notify();
            },
        );
        let on_release = cx.listener(
            move |this: &mut MainWindow, _: &gpui::MouseUpEvent, window, cx| {
                if this.armed(id) {
                    action(this, window, cx);
                }
            },
        );
        // Layout size stays put. The disc and glyph scale in paint, so they
        // do not step across device pixels.
        div()
            .id(id)
            .size(px(size))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .on_hover(on_hover)
            .on_mouse_down(MouseButton::Left, on_press)
            .on_mouse_up(MouseButton::Left, on_release)
            .child(div().with_spring(
                SharedString::from(format!("{id}-press")),
                Self::hover_spring(pressed),
                move |frame, phase| {
                    let scale = Self::press_scale(phase);
                    let mark = match swap {
                        Some((other, show_other)) => {
                            crossfade_icon(id, glyph, other, show_other, color, icon_size, scale)
                        }
                        None => icon_scaled(glyph, color, px(icon_size), scale).into_any_element(),
                    };
                    frame.child(button_face(size, scale, hover_id.clone(), hovered, mark))
                },
            ))
            .into_any_element()
    }

    fn mode_button(
        &self,
        id: &'static str,
        glyph: &'static [u8],
        active: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut MainWindow, &mut gpui::Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        self.glyph_button(
            id,
            glyph,
            None,
            32.,
            15.,
            if active {
                theme::accent()
            } else {
                theme::text_secondary()
            },
            cx,
            action,
        )
    }

    fn volume_control(&mut self, volume: f32, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .items_center()
            .justify_end()
            .gap(px(12.))
            .child({
                let id = "mute";
                let hovered = self.hovered_ui.as_deref() == Some(id);
                let pressed = self.pressed_ui.as_deref() == Some(id);
                let hover_id = SharedString::from(format!("{id}-hover"));
                let volume = volume;
                div()
                    .id(id)
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .cursor_pointer()
                    .on_hover(Self::hover_listener(SharedString::from(id), cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.arm("mute", true);
                            cx.notify();
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                            if !this.armed("mute") {
                                return;
                            }
                            this.session.update(cx, |session, cx| {
                                session.toggle_mute();
                                cx.notify();
                            });
                        }),
                    )
                    .child(div().with_spring(
                        SharedString::from("mute-press"),
                        Self::hover_spring(pressed),
                        move |frame, phase| {
                            let scale = Self::press_scale(phase);
                            frame.child(button_face(
                                32.,
                                scale,
                                hover_id.clone(),
                                hovered,
                                volume_icon(volume, scale),
                            ))
                        },
                    ))
            })
            .child(self.slider("volume", volume, SliderDrag::Volume, theme::accent(), cx))
    }

    fn progress_bar(
        &mut self,
        progress: f32,
        time: &str,
        duration: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .pb(px(4.))
            .child(seek_time(time, &time_slot(time), true))
            .child(div().flex_1().min_w(px(0.)).child(self.slider(
                "progress",
                progress,
                SliderDrag::Progress,
                theme::accent(),
                cx,
            )))
            .child(seek_time(duration, &time_slot(duration), false))
    }

    fn slider(
        &mut self,
        id: &'static str,
        value: f32,
        kind: SliderDrag,
        fill: impl Into<gpui::Hsla>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let fill = fill.into();
        let this = cx.entity().clone();
        let kind_for_bounds = kind;
        let dragging = self.drag == Some(kind);
        let hover_id = SharedString::from(id);
        let show_thumb = dragging || self.hovered_ui.as_ref() == Some(&hover_id);
        div()
            .id(id)
            .group(id)
            .on_hover(Self::hover_listener(hover_id, cx))
            .relative()
            .w(px(if kind == SliderDrag::Volume { 100. } else { 0. }))
            .when(kind == SliderDrag::Progress, |bar| bar.w_full())
            .h(px(14.))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.drag = Some(kind);
                    this.drag_to(event.position.x, cx);
                }),
            )
            .on_scroll_wheel(
                cx.listener(move |this, event: &gpui::ScrollWheelEvent, _, cx| {
                    let delta_y = match event.delta {
                        gpui::ScrollDelta::Lines(point) => point.y,
                        gpui::ScrollDelta::Pixels(point) => point.y.as_f32(),
                    };
                    if delta_y == 0.0 {
                        return;
                    }
                    // Windows reports a wheel notch away from the user as
                    // positive. One notch is a fixed step, not the line count:
                    // volume ±5%, seek ±5 seconds. Up is louder and later.
                    cx.stop_propagation();
                    match kind {
                        SliderDrag::Volume => {
                            let step = if delta_y > 0.0 { 0.05 } else { -0.05 };
                            this.session.update(cx, |session, cx| {
                                session.set_volume(session.volume + step);
                                cx.notify();
                            });
                        }
                        SliderDrag::Progress => {
                            let step = if delta_y > 0.0 { 5.0 } else { -5.0 };
                            this.session.update(cx, |session, cx| {
                                session.seek_by(step);
                                cx.notify();
                            });
                        }
                    }
                }),
            )
            .child(
                canvas(
                    move |bounds, _window, app| {
                        this.update(app, |view, _cx| {
                            view.set_slider_bounds(kind_for_bounds, bounds)
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .right(px(0.))
                .bottom(px(0.)),
            )
            .child({
                let (track_h, track_top) = if kind == SliderDrag::Progress {
                    (px(4.), px(4.5))
                } else {
                    (px(4.), px(5.5))
                };
                div()
                    .absolute()
                    .left(px(0.))
                    .right(px(0.))
                    .top(track_top)
                    .h(track_h)
                    .rounded_full()
                    .bg(theme::bg_elevated())
                    .child(div().h_full().w(relative(value)).rounded_full().bg(fill))
            })
            .child({
                let thumb_top = if kind == SliderDrag::Progress {
                    px(0.5)
                } else {
                    px(1.5)
                };
                div()
                    .absolute()
                    .top(thumb_top)
                    .left(relative(value))
                    .ml(px(-6.))
                    .size(px(12.))
                    .rounded_full()
                    .bg(gpui::rgb(0xffffff))
                    .with_spring(
                        SharedString::from(format!("{id}-thumb")),
                        Self::hover_spring(show_thumb),
                        |thumb, phase| thumb.opacity(phase.0),
                    )
            })
            .into_any_element()
    }

    pub(crate) fn set_slider_bounds(&mut self, kind: SliderDrag, bounds: Bounds<Pixels>) {
        match kind {
            SliderDrag::Progress => self.progress_bounds = bounds,
            SliderDrag::Volume => self.volume_bounds = bounds,
        }
    }

    pub(crate) fn drag_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(kind) = self.drag else {
            return;
        };
        let bounds = match kind {
            SliderDrag::Progress => self.progress_bounds,
            SliderDrag::Volume => self.volume_bounds,
        };
        let fraction = fraction_at(bounds, x);
        self.session.update(cx, |session, cx| {
            match kind {
                SliderDrag::Progress => session.seek(fraction),
                SliderDrag::Volume => session.set_volume(fraction),
            }
            cx.notify();
        });
    }
}

fn time_slot(label: &str) -> String {
    label
        .chars()
        .map(|ch| if ch == ':' { ':' } else { '0' })
        .collect()
}

fn seek_time(label: &str, slot: &str, align_end: bool) -> impl IntoElement {
    // The slot is a zeroed copy of this label, drawn with tabular figures.
    // Each side keeps its own width, so a short elapsed time is not padded
    // out to an hour-long duration.
    let features = gpui::FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]));
    div()
        .relative()
        .flex_shrink_0()
        .child(time_text(slot, features.clone()).invisible())
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .when(align_end, |row| row.justify_end())
                .child(time_text(label, features)),
        )
}

fn time_text(label: &str, features: gpui::FontFeatures) -> gpui::Div {
    div()
        .text_size(px(10.))
        .text_color(theme::text_secondary())
        .font_features(features)
        .child(label.to_string())
}

fn crossfade_icon(
    id: &'static str,
    off: &'static [u8],
    on: &'static [u8],
    show_on: bool,
    color: gpui::Hsla,
    icon_size: f32,
    scale: f32,
) -> AnyElement {
    div()
        .relative()
        .size(px(icon_size))
        .with_spring(
            SharedString::from(format!("{id}-glyph")),
            MainWindow::hover_spring(show_on),
            move |stack, phase| {
                let t = phase.0.clamp(0.0, 1.0);
                stack
                    .child(faded_glyph(off, color, icon_size, scale, 1.0 - t))
                    .child(faded_glyph(on, color, icon_size, scale, t))
            },
        )
        .into_any_element()
}

fn faded_glyph(
    data: &'static [u8],
    color: gpui::Hsla,
    icon_size: f32,
    scale: f32,
    opacity: f32,
) -> AnyElement {
    div()
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .size(px(icon_size))
        .flex()
        .items_center()
        .justify_center()
        .opacity(opacity)
        .child(icon_scaled(data, color, px(icon_size), scale))
        .into_any_element()
}

fn button_face(
    size: f32,
    scale: f32,
    hover_id: SharedString,
    hovered: bool,
    mark: impl IntoElement,
) -> impl IntoElement {
    div()
        .relative()
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .with_spring(
                    hover_id,
                    MainWindow::hover_spring(hovered),
                    move |layer, phase| {
                        layer.child(scaled_disc(
                            theme::hover().opacity(phase.0),
                            px(size),
                            scale,
                        ))
                    },
                ),
        )
        .child(mark)
}

fn volume_icon(volume: f32, scale: f32) -> impl IntoElement {
    let color = theme::text_secondary();
    let size = px(16.);
    let layer = |glyph| {
        div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .size(size)
            .flex()
            .items_center()
            .justify_center()
            .child(icon_scaled(glyph, color, size, scale))
    };
    div()
        .relative()
        .size(size)
        .child(icon_scaled(icons::VOL_MIN, color, size, scale))
        .when(volume > 0.33, |stack| stack.child(layer(icons::VOL_MED)))
        .when(volume > 0.66, |stack| stack.child(layer(icons::VOL_MAX)))
        .when(volume <= 0.0, |stack| stack.child(layer(icons::MUTE)))
}

fn fraction_at(bounds: Bounds<Pixels>, x: Pixels) -> f32 {
    let width = bounds.size.width;
    if width < px(1.) {
        return 0.0;
    }
    ((x - bounds.left()) / width).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{limit_label, time_slot};

    #[test]
    fn transport_labels_stop_at_22_chars() {
        assert_eq!(limit_label("короткий"), "короткий");
        assert_eq!(limit_label(&"a".repeat(22)), "a".repeat(22));
        let limited = limit_label("абвгдеёжзийклмнопрстуфхцчшщъыьэюя");
        assert_eq!(limited.chars().count(), 22);
        assert!(limited.ends_with('…'));
        assert_eq!(
            &limited[..limited.len() - "…".len()],
            "абвгдеёжзийклмнопрсту"
        );
    }

    #[test]
    fn elapsed_time_is_not_padded_to_the_duration() {
        assert_eq!(time_slot("0:12"), "0:00");
        assert_eq!(time_slot("1:02:03"), "0:00:00");
        assert_ne!(time_slot("0:12").len(), time_slot("1:02:03").len());
    }
}
