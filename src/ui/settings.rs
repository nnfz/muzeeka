mod audio;

use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, Context, FocusHandle, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Render, Styled, Task, TitlebarOptions, Window, WindowBounds, WindowControlArea,
    WindowHandle, WindowOptions, div, prelude::*, px, size,
};

use crate::ui::icons::{self, icon};
use crate::ui::main_window::alt_wheel_layer;
use crate::ui::session::{PlaylistDensity, Session, SettingsSection};
use crate::ui::text_field::TextField;
use crate::ui::theme;

pub struct SettingsWindow {
    session: gpui::Entity<Session>,
    focus: FocusHandle,
    section: SettingsSection,
    use_ui_font: bool,
    /// Control that received the mouse down. Same reason as MainWindow::armed_ui:
    /// GPUI on_click clears hover for the whole hold.
    armed: Option<gpui::SharedString>,
    audio: audio::AudioUi,
    /// Name typed while saving an effect or chain preset.
    preset_name: gpui::Entity<TextField>,
    _meter: Task<()>,
}

impl SettingsWindow {
    pub fn new(session: gpui::Entity<Session>, use_ui_font: bool, cx: &mut Context<Self>) -> Self {
        Self {
            session,
            focus: cx.focus_handle(),
            section: SettingsSection::General,
            use_ui_font,
            armed: None,
            audio: audio::AudioUi::new(),
            preset_name: cx.new(|cx| {
                TextField::new(cx, "Preset name", |_text, _window, _app| {}, |_window, _app| {})
            }),
            _meter: cx.spawn(async move |this, cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if this
                    .update(cx, |view, cx| {
                        if view.poll_meters(cx) {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }),
        }
    }

    fn arm(&mut self, id: impl Into<gpui::SharedString>) {
        self.armed = Some(id.into());
    }

    fn armed(&self, id: &str) -> bool {
        self.armed.as_deref() == Some(id)
    }

    pub fn open(
        session: gpui::Entity<Session>,
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
        let bounds = Bounds::centered(None, size(px(960.), px(620.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Settings".into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                window_min_size: Some(size(px(786.), px(480.))),
                is_minimizable: false,
                ..Default::default()
            },
            move |_window, cx| cx.new(|cx| Self::new(session, use_ui_font, cx)),
        )
        .ok()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let section = self.section;
        let session = self.session.clone();
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::bg_deep())
            .text_color(theme::text())
            .when(self.use_ui_font, |root| root.font_family(theme::FONT_FAMILY))
            .child(alt_wheel_layer(session))
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus.focus(window, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if event.dragging() {
                    this.audio_drag_move(event.position, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.end_audio_drag(cx);
                    this.armed = None;
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, window, cx| {
                    this.end_audio_drag(cx);
                    // An occluding control (the close button) makes the root
                    // look unhovered even when the pointer is still inside.
                    // Dropping the press there cancels the click.
                    let size = window.viewport_size();
                    let inside = event.position.x >= px(0.)
                        && event.position.y >= px(0.)
                        && event.position.x < size.width
                        && event.position.y < size.height;
                    if !inside {
                        this.armed = None;
                    }
                }),
            )
            .child(self.settings_header(window, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .gap(px(8.))
                    .px(px(8.))
                    .pb(px(8.))
                    .child(self.section_list(cx))
                    .child(self.section_body(section, cx)),
            )
            .child(self.preset_popup(cx))
    }
}

impl SettingsWindow {
    fn settings_header(&self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
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
                    .child("Settings"),
            )
            .child(
                // Windows hit-tests the first caption area under the cursor.
                // The header is Drag, so this button has to occlude it and
                // report Close. A mouse handler that stops the press keeps
                // the system from posting WM_CLOSE, and the window stays open.
                div()
                    .id("settings-close")
                    .occlude()
                    .window_control_area(WindowControlArea::Close)
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(32.))
                    .rounded_md()
                    .cursor_pointer()
                    .child(icon(icons::CLOSE, theme::text_secondary(), px(10.)))
                    .hover(|style| style.bg(theme::danger())),
            )
    }

    fn section_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(200.))
            .h_full()
            .rounded_md()
            .bg(theme::bg_surface())
            .border_1()
            .border_color(theme::border())
            .py(px(8.))
            .children(SettingsSection::ALL.map(|section| {
                let active = self.section == section;
                let section_id = gpui::SharedString::from(format!("settings-{}", section.label()));
                let arm_id = section_id.clone();
                let release_id = section_id.clone();
                div()
                    .id(section_id)
                    .mx(px(6.))
                    .h(px(34.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(if active {
                        theme::text()
                    } else {
                        theme::text_secondary()
                    })
                    .when(active, |row| row.bg(theme::accent_soft()))
                    .hover(|style| style.bg(if active { theme::accent_soft() } else { theme::hover() }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                            this.arm(arm_id.clone());
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                            if !this.armed(&release_id) {
                                return;
                            }
                            this.section = section;
                            cx.notify();
                        }),
                    )
                    .child(section.label())
            }))
    }

    fn section_body(&self, section: SettingsSection, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .rounded_md()
            .bg(theme::bg_surface())
            .border_1()
            .border_color(theme::border())
            .id("settings-body")
            .overflow_y_scroll()
            .p(px(16.))
            .child(match section {
                SettingsSection::General => self.general(cx).into_any_element(),
                SettingsSection::Downloads => static_section(
                    "Downloads",
                    "Where downloaded tracks are saved and which playlist receives them.",
                    "Download folder",
                    "App data / downloads",
                )
                .into_any_element(),
                SettingsSection::Plugins => static_section(
                    "Plugins",
                    "Extensions loaded from the plugins folder.",
                    "Installed",
                    "No plugins are listed until the plugin host is ported.",
                )
                .into_any_element(),
                SettingsSection::Audio => self.audio_section(cx),
                SettingsSection::About => static_section(
                    "About",
                    "Muzeeka native",
                    "Version",
                    "0.1.0",
                )
                .into_any_element(),
            })
    }

    fn general(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (discord, video, smart, density) = {
            let session = self.session.read(cx);
            (
                session.discord_rpc,
                session.auto_video_bg,
                session.shuffle_smart,
                session.playlist_density,
            )
        };

        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(section_heading(
                "General",
                "App behavior and integrations.",
            ))
            .child(
                card()
                    .child(self.toggle_row(
                        "discord-rpc",
                        "Discord Rich Presence",
                        "Show the current track in Discord",
                        discord,
                        cx,
                        |session| session.discord_rpc = !session.discord_rpc,
                    ))
                    .child(self.toggle_row(
                        "auto-video",
                        "Auto video backgrounds",
                        "Automatically download 15-second YouTube clips for track backgrounds",
                        video,
                        cx,
                        |session| session.auto_video_bg = !session.auto_video_bg,
                    ))
                    .child(self.density_row(density, cx))
                    .child(self.shuffle_row(smart, cx)),
            )
    }

    fn density_row(&self, density: PlaylistDensity, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .py(px(10.))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.))
                    .child(div().text_size(px(13.)).child("Playlist layout"))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme::text_muted())
                            .child(density.detail()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .rounded_md()
                    .bg(theme::bg_deep())
                    .p(px(2.))
                    .children(PlaylistDensity::ALL.map(|mode| {
                        self.density_chip(
                            match mode {
                                PlaylistDensity::Normal => "density-normal",
                                PlaylistDensity::Compact => "density-compact",
                                PlaylistDensity::Nano => "density-nano",
                            },
                            mode,
                            density == mode,
                            cx,
                        )
                    })),
            )
            .into_any_element()
    }

    fn density_chip(
        &self,
        id: &'static str,
        mode: PlaylistDensity,
        active: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(id)
            .px(px(10.))
            .h(px(26.))
            .flex()
            .items_center()
            .rounded_sm()
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(if active {
                theme::text()
            } else {
                theme::text_muted()
            })
            .when(active, |chip| chip.bg(theme::bg_elevated()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                    this.arm(id);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if !this.armed(id) {
                        return;
                    }
                    this.session.update(cx, |session, cx| {
                        session.playlist_density = mode;
                        cx.notify();
                    });
                }),
            )
            .child(mode.label())
            .into_any_element()
    }

    fn toggle_row(
        &self,
        id: &'static str,
        label: &'static str,
        detail: &'static str,
        on: bool,
        cx: &mut Context<Self>,
        toggle: impl Fn(&mut Session) + 'static,
    ) -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .py(px(10.))
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.))
                    .child(div().text_size(px(13.)).child(label))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme::text_muted())
                            .child(detail),
                    ),
            )
            .child(switch(id, on, cx, toggle))
            .into_any_element()
    }

    fn shuffle_row(&self, smart: bool, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .py(px(10.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(13.)).child("Shuffle mode"))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme::text_muted())
                            .child(if smart {
                                "Smart: remembers tracks already played in this playlist"
                            } else {
                                "Normal: classic random order"
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .rounded_md()
                    .bg(theme::bg_deep())
                    .p(px(2.))
                    .child(self.mode_chip("shuffle-smart", "Smart", smart, cx, true))
                    .child(self.mode_chip("shuffle-normal", "Normal", !smart, cx, false)),
            )
            .into_any_element()
    }

    fn mode_chip(
        &self,
        id: &'static str,
        label: &'static str,
        active: bool,
        cx: &mut Context<Self>,
        smart: bool,
    ) -> AnyElement {
        div()
            .id(id)
            .px(px(10.))
            .h(px(26.))
            .flex()
            .items_center()
            .rounded_sm()
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(if active {
                theme::text()
            } else {
                theme::text_muted()
            })
            .when(active, |chip| chip.bg(theme::bg_elevated()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                    this.arm(id);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if !this.armed(id) {
                        return;
                    }
                    this.session.update(cx, |session, cx| {
                        session.shuffle_smart = smart;
                        cx.notify();
                    });
                }),
            )
            .child(label)
            .into_any_element()
    }
}

fn switch(
    id: &'static str,
    on: bool,
    cx: &mut Context<SettingsWindow>,
    toggle: impl Fn(&mut Session) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .w(px(36.))
        .h(px(20.))
        .rounded_full()
        .p(px(2.))
        .cursor_pointer()
        .bg(if on { theme::accent() } else { theme::bg_elevated() })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                this.arm(id);
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                if !this.armed(id) {
                    return;
                }
                this.session.update(cx, |session, cx| {
                    toggle(session);
                    cx.notify();
                });
            }),
        )
        .child(
            div()
                .size(px(16.))
                .rounded_full()
                .bg(theme::white())
                .when(on, |knob| knob.ml(px(16.))),
        )
        .into_any_element()
}

fn section_heading(title: &'static str, detail: &'static str) -> AnyElement {
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

fn card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .rounded_md()
        .bg(theme::bg_deep())
        .px(px(12.))
}

fn static_section(
    title: &'static str,
    detail: &'static str,
    label: &'static str,
    value: &'static str,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(section_heading(title, detail))
        .child(
            card()
                .py(px(12.))
                .child(div().text_size(px(13.)).child(label))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child(value),
                ),
        )
}
