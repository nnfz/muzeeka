use gpui::{
    div, px, AnyElement, Context, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Styled, Window, WindowControlArea,
};

use crate::ui::icons::{self, icon};
use crate::ui::main_window::MainWindow;
use crate::ui::theme;

impl MainWindow {
    pub(crate) fn header_buttons(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let maximized = window.is_maximized();
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(self.settings_button(cx))
            .child(self.caption_button(
                "win-min",
                icons::MINIMIZE,
                false,
                WindowControlArea::Min,
            ))
            .child(self.caption_button(
                "win-max",
                if maximized {
                    icons::RESTORE
                } else {
                    icons::MAXIMIZE
                },
                false,
                WindowControlArea::Max,
            ))
            .child(self.close_button(cx))
    }

    fn settings_button(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("open-settings")
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .size(px(32.))
            .rounded_md()
            .cursor_pointer()
            .child(icon(icons::OPTIONS, theme::text_secondary(), px(16.)))
            .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.arm("open-settings", false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, window, cx| {
                    if this.armed("open-settings") {
                        this.open_settings(window, cx);
                    }
                }),
            )
            .into_any_element()
    }

    fn caption_button(
        &self,
        id: &'static str,
        glyph: &'static [u8],
        close: bool,
        area: WindowControlArea,
    ) -> AnyElement {
        self.chrome_button(id, glyph, close)
            .occlude()
            .window_control_area(area)
            .into_any_element()
    }

    fn close_button(&self, cx: &mut Context<Self>) -> AnyElement {
        self.chrome_button("win-close", icons::CLOSE, true)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.arm("win-close", false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.armed("win-close") {
                        cx.quit();
                    }
                }),
            )
            .into_any_element()
    }

    fn chrome_button(
        &self,
        id: &'static str,
        glyph: &'static [u8],
        close: bool,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(32.))
            .rounded_md()
            .cursor_pointer()
            .child(icon(glyph, theme::text_secondary(), px(10.)))
            .hover(move |style| {
                if close {
                    style.bg(theme::danger()).text_color(theme::white())
                } else {
                    style.bg(theme::hover()).text_color(theme::text())
                }
            })
    }
}
