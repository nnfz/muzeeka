use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    actions, div, fill, point, px, relative, size, App, Bounds, ClipboardItem, Context,
    CursorStyle, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle,
    GlobalElementId, InspectorElementId, InteractiveElement, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, ParentElement, Pixels, Point,
    ShapedLine, SharedString, Styled, TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window,
};

use crate::ui::theme;

actions!(
    search_field,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        EscapeField,
        Submit,
        AcceptSuggestion,
        PickUp,
        PickDown,
    ]
);

pub struct TextField {
    pub focus: FocusHandle,
    pub content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    element_id: SharedString,
    on_change: Rc<dyn Fn(&str, &mut Window, &mut App)>,
    on_escape: Rc<dyn Fn(&mut Window, &mut App)>,
    on_submit: Rc<dyn Fn(&str, &mut Window, &mut App)>,
    on_tab: Rc<dyn Fn(&str, &mut Window, &mut App) -> bool>,
    on_pick: Rc<dyn Fn(&str, i32, &mut Window, &mut App) -> bool>,
    picks: bool,
    multiline: bool,
    line_starts: Vec<usize>,
    line_layouts: Vec<ShapedLine>,
    scroll_y: Pixels,
    line_h: Pixels,
}

impl TextField {
    pub fn new(
        cx: &mut Context<Self>,
        placeholder: impl Into<SharedString>,
        on_change: impl Fn(&str, &mut Window, &mut App) + 'static,
        on_escape: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            focus: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            selecting: false,
            element_id: "search-field".into(),
            on_change: Rc::new(on_change),
            on_escape: Rc::new(on_escape),
            on_submit: Rc::new(|_text, _window, _app| {}),
            on_tab: Rc::new(|_text, _window, _app| false),
            on_pick: Rc::new(|_text, _delta, _window, _app| false),
            picks: false,
            multiline: false,
            line_starts: Vec::new(),
            line_layouts: Vec::new(),
            scroll_y: px(0.),
            line_h: px(0.),
        }
    }

    pub fn multiline(mut self) -> Self {
        self.multiline = true;
        self
    }

    pub fn element_id(mut self, id: impl Into<SharedString>) -> Self {
        self.element_id = id.into();
        self
    }

    pub fn set_on_submit(&mut self, submit: impl Fn(&str, &mut Window, &mut App) + 'static) {
        self.on_submit = Rc::new(submit);
    }

    pub fn set_on_tab(&mut self, accept: impl Fn(&str, &mut Window, &mut App) -> bool + 'static) {
        self.on_tab = Rc::new(accept);
    }

    pub fn set_on_pick(
        &mut self,
        pick: impl Fn(&str, i32, &mut Window, &mut App) -> bool + 'static,
    ) {
        self.on_pick = Rc::new(pick);
        self.picks = true;
    }

    pub fn set_on_escape(&mut self, escape: impl Fn(&mut Window, &mut App) + 'static) {
        self.on_escape = Rc::new(escape);
    }

    pub fn replace_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        let end = text.len();
        self.content = text.into();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    pub fn set_text(
        &mut self,
        text: impl Into<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = text.into();
        let end = text.len();
        self.content = text.into();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.emit(window, cx);
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.replace_text("", cx);
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn emit(&self, window: &mut Window, cx: &mut Context<Self>) {
        let notify = self.on_change.clone();
        let text = self.content.clone();
        notify(&text, window, cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            let (start, _) = self.line_span(self.cursor_offset());
            self.move_to(start, cx);
            return;
        }
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            let (_, end) = self.line_span(self.cursor_offset());
            self.move_to(end, cx);
            return;
        }
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let text = self.normalize_input(&text);
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn escape(&mut self, _: &EscapeField, window: &mut Window, cx: &mut Context<Self>) {
        let escape = self.on_escape.clone();
        window.defer(cx, move |window, cx| {
            escape(window, cx);
        });
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.replace_text_in_range(None, "\n", window, cx);
            return;
        }
        let text = self.content.to_string();
        let submit = self.on_submit.clone();
        submit(&text, window, cx);
    }

    fn accept_suggestion(
        &mut self,
        _: &AcceptSuggestion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let accept = self.on_tab.clone();
        let text = self.content.to_string();
        if !accept(&text, window, cx) {
            cx.propagate();
        }
    }

    fn pick_up(&mut self, _: &PickUp, window: &mut Window, cx: &mut Context<Self>) {
        self.pick(-1, window, cx);
    }

    fn pick_down(&mut self, _: &PickDown, window: &mut Window, cx: &mut Context<Self>) {
        self.pick(1, window, cx);
    }

    fn pick(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            let pick = self.on_pick.clone();
            let text = self.content.to_string();
            if self.picks && pick(&text, delta, window, cx) {
                return;
            }
            self.move_line(delta, cx);
            return;
        }
        let pick = self.on_pick.clone();
        let text = self.content.to_string();
        if !pick(&text, delta, window, cx) {
            cx.propagate();
        }
    }

    fn normalize_input(&self, text: &str) -> String {
        if self.multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace(['\n', '\r'], " ")
        }
    }

    fn line_span(&self, offset: usize) -> (usize, usize) {
        let mut start = 0usize;
        for (index, ch) in self.content.char_indices() {
            if ch == '\n' {
                if offset <= index {
                    return (start, index);
                }
                start = index + ch.len_utf8();
            }
        }
        (start, self.content.len())
    }

    fn line_index_at(&self, offset: usize) -> usize {
        self.line_starts
            .iter()
            .enumerate()
            .rev()
            .find(|(_, start)| **start <= offset)
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    fn move_line(&mut self, delta: i32, cx: &mut Context<Self>) {
        if self.line_starts.is_empty() {
            return;
        }
        let offset = self.cursor_offset();
        let current = self.line_index_at(offset);
        let next = current as i32 + delta;
        if next < 0 {
            self.move_to(0, cx);
            return;
        }
        if next as usize >= self.line_starts.len() {
            self.move_to(self.content.len(), cx);
            return;
        }
        let next = next as usize;
        let current_start = self.line_starts[current];
        let x = self
            .line_layouts
            .get(current)
            .map(|line| line.x_for_index(offset.saturating_sub(current_start)))
            .unwrap_or(px(0.));
        let start = self.line_starts[next];
        let boundary = self
            .line_starts
            .get(next + 1)
            .copied()
            .unwrap_or(self.content.len());
        let end = if boundary > start && self.content.as_bytes().get(boundary - 1) == Some(&b'\n') {
            boundary - 1
        } else {
            boundary
        };
        let rel = self
            .line_layouts
            .get(next)
            .map(|line| line.closest_index_for_x(x))
            .unwrap_or(0);
        let dest = start.saturating_add(rel).min(end);
        self.move_to(dest, cx);
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selecting = true;
        let index = self.index_for_position(event.position);
        if event.modifiers.shift {
            self.select_to(index, cx);
        } else {
            self.move_to(index, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            self.select_to(self.index_for_position(event.position), cx);
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        if self.multiline {
            return self.index_for_multiline(position);
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if self.content.is_empty() {
            return 0;
        }
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn index_for_multiline(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        if self.content.is_empty() || self.line_starts.is_empty() {
            return 0;
        }
        if position.y < bounds.top() {
            return 0;
        }
        let line_h = if self.line_h > px(0.) {
            self.line_h
        } else {
            px(16.)
        };
        let y = (position.y - bounds.top() + self.scroll_y).max(px(0.));
        let index = (y.as_f32() / line_h.as_f32().max(1.0)).floor() as usize;
        if index >= self.line_starts.len() {
            return self.content.len();
        }
        if position.y > bounds.bottom() {
            let content_h = line_h.as_f32() * self.line_starts.len() as f32;
            if self.scroll_y.as_f32() + (position.y - bounds.top()).as_f32() >= content_h {
                return self.content.len();
            }
        }
        let start = self.line_starts[index];
        let boundary = self
            .line_starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.content.len());
        let end = if boundary > start && self.content.as_bytes().get(boundary - 1) == Some(&b'\n') {
            boundary - 1
        } else {
            boundary
        };
        if end == start {
            return start;
        }
        let Some(layout) = self.line_layouts.get(index) else {
            return start;
        };
        let rel = layout
            .closest_index_for_x(position.x - bounds.left())
            .min(end - start);
        start + rel
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify();
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .get(..offset)
            .and_then(|text| text.char_indices().next_back().map(|(index, _)| index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .get(offset..)
            .and_then(|text| text.chars().next().map(|ch| offset + ch.len_utf8()))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        self.content.get(range).map(|text| text.to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let new_text = self.normalize_input(new_text);
        let next = self.content.get(..range.start).unwrap_or("").to_owned()
            + &new_text
            + self.content.get(range.end..).unwrap_or("");
        self.content = next.into();
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        self.marked_range = None;
        self.emit(window, cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let new_text = self.normalize_input(new_text);
        let next = self.content.get(..range.start).unwrap_or("").to_owned()
            + &new_text
            + self.content.get(range.end..).unwrap_or("");
        self.content = next.into();
        if new_text.is_empty() {
            self.marked_range = None;
        } else {
            self.marked_range = Some(range.start..range.start + new_text.len());
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.start)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.emit(window, cx);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if self.multiline {
            if self.line_starts.is_empty() {
                return Some(bounds);
            }
            let line = self.line_index_at(range.start);
            let start = self.line_starts.get(line).copied().unwrap_or(0);
            let layout = self.line_layouts.get(line)?;
            let y = bounds.top() - self.scroll_y + self.line_h * line as f32;
            return Some(Bounds::from_corners(
                point(
                    bounds.left() + layout.x_for_index(range.start.saturating_sub(start)),
                    y,
                ),
                point(
                    bounds.left() + layout.x_for_index(range.end.saturating_sub(start)),
                    y + self.line_h,
                ),
            ));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        if self.multiline {
            return Some(self.offset_to_utf16(self.index_for_multiline(point)));
        }
        let bounds = self.last_bounds?;
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout.index_for_x(point.x - bounds.left())?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextField>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    lines: Vec<ShapedLine>,
    line_starts: Vec<usize>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
    selections: Vec<PaintQuad>,
    scroll: Pixels,
    scroll_y: Pixels,
    line_h: Pixels,
    multiline: bool,
    blanks: Vec<bool>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let multiline = self.input.read(cx).multiline;
        let mut style = gpui::Style::default();
        style.size.width = relative(1.).into();
        style.size.height = if multiline {
            relative(1.).into()
        } else {
            window.line_height().into()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        if input.multiline {
            return self.prepaint_lines(bounds, window, cx);
        }
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let placeholder = content.is_empty();
        let (display_text, text_color) = if placeholder {
            (input.placeholder.clone(), theme::text_muted().into())
        } else {
            (content, style.color)
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let Some(marked_range) = input.marked_range.clone() {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end.saturating_sub(marked_range.start),
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len().saturating_sub(marked_range.end),
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);
        let cursor_x = if placeholder {
            px(0.)
        } else {
            line.x_for_index(cursor)
        };
        let scroll = (cursor_x - (bounds.size.width - px(4.))).max(px(0.));
        let origin_x = bounds.left() - scroll;

        let (selection, cursor_quad) = if placeholder || selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(origin_x + cursor_x, bounds.top()),
                        size(px(1.5), bounds.size.height),
                    ),
                    theme::accent(),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            origin_x + line.x_for_index(selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            origin_x + line.x_for_index(selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    theme::accent_soft(),
                )),
                None,
            )
        };

        PrepaintState {
            line: Some(line),
            lines: Vec::new(),
            line_starts: Vec::new(),
            cursor: cursor_quad,
            selection,
            selections: Vec::new(),
            scroll,
            scroll_y: px(0.),
            line_h: px(0.),
            multiline: false,
            blanks: Vec::new(),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.input.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if prepaint.multiline {
            for selection in prepaint.selections.drain(..) {
                window.paint_quad(selection);
            }
            let line_h = prepaint.line_h;
            let scroll_y = prepaint.scroll_y;
            let blanks = std::mem::take(&mut prepaint.blanks);
            let lines = std::mem::take(&mut prepaint.lines);
            for (index, line) in lines.iter().enumerate() {
                let y = bounds.top() - scroll_y + line_h * index as f32;
                if y + line_h < bounds.top() || y > bounds.bottom() {
                    continue;
                }
                if blanks.get(index).copied().unwrap_or(false) {
                    continue;
                }
                let _ = line.paint(
                    point(bounds.left(), y),
                    line_h,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            if focus.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
            let starts = std::mem::take(&mut prepaint.line_starts);
            self.input.update(cx, |input, _cx| {
                input.line_layouts = lines;
                input.line_starts = starts;
                input.scroll_y = scroll_y;
                input.line_h = line_h;
                input.last_bounds = Some(bounds);
                input.last_layout = None;
            });
            return;
        }
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        let line = prepaint.line.take().unwrap();
        let origin = point(bounds.left() - prepaint.scroll, bounds.top());
        let _ = line.paint(
            origin,
            window.line_height(),
            TextAlign::Left,
            None,
            window,
            cx,
        );
        if focus.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }
        self.input.update(cx, |input, _cx| {
            input.last_layout = Some(line);
            input.last_bounds = Some(Bounds::new(origin, bounds.size));
        });
    }
}

impl TextElement {
    fn prepaint_lines(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_h = window.line_height();
        let placeholder = content.is_empty();
        let source = if placeholder {
            input.placeholder.to_string()
        } else {
            content.to_string()
        };
        let color = if placeholder {
            theme::text_muted().into()
        } else {
            style.color
        };
        let mut line_starts = Vec::new();
        let mut lines = Vec::new();
        let mut empty = Vec::new();
        if placeholder {
            line_starts.push(0);
            empty.push(false);
            let run = TextRun {
                len: source.len(),
                font: style.font(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            lines.push(window.text_system().shape_line(
                SharedString::from(source.as_str()),
                font_size,
                &[run],
                None,
            ));
        } else {
            let mut offset = 0usize;
            let mut rest = source.as_str();
            loop {
                let (line, next) = match rest.split_once('\n') {
                    Some((line, next)) => (line, Some(next)),
                    None => (rest, None),
                };
                line_starts.push(offset);
                empty.push(line.is_empty());
                let display: SharedString = if line.is_empty() {
                    " ".into()
                } else {
                    line.to_string().into()
                };
                let run = TextRun {
                    len: display.len(),
                    font: style.font(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                lines.push(
                    window
                        .text_system()
                        .shape_line(display, font_size, &[run], None),
                );
                offset += line.len() + 1;
                match next {
                    Some(next) => rest = next,
                    None => break,
                }
            }
        }
        let cursor_line = if placeholder {
            0
        } else {
            line_starts
                .iter()
                .enumerate()
                .rev()
                .find(|(_, start)| **start <= cursor)
                .map(|(index, _)| index)
                .unwrap_or(0)
        };
        let mut scroll_y = input.scroll_y;
        let view_h = bounds.size.height.as_f32();
        let cursor_top = line_h.as_f32() * cursor_line as f32;
        if cursor_top < scroll_y.as_f32() {
            scroll_y = px(cursor_top);
        }
        let cursor_bottom = cursor_top + line_h.as_f32();
        if cursor_bottom > scroll_y.as_f32() + view_h {
            scroll_y = px((cursor_bottom - view_h).max(0.0));
        }
        let mut selections = Vec::new();
        if !placeholder && !selected_range.is_empty() {
            for (index, start) in line_starts.iter().enumerate() {
                let boundary = line_starts.get(index + 1).copied().unwrap_or(source.len());
                let line_end =
                    if boundary > *start && source.as_bytes().get(boundary - 1) == Some(&b'\n') {
                        boundary - 1
                    } else {
                        boundary
                    };
                let sel_start = selected_range.start.max(*start);
                let sel_end = selected_range.end.min(line_end);
                if sel_start >= sel_end {
                    continue;
                }
                let y = bounds.top() - scroll_y + line_h * index as f32;
                let layout = &lines[index];
                selections.push(fill(
                    Bounds::from_corners(
                        point(bounds.left() + layout.x_for_index(sel_start - start), y),
                        point(
                            bounds.left() + layout.x_for_index(sel_end - start),
                            y + line_h,
                        ),
                    ),
                    theme::accent_soft(),
                ));
            }
        }
        let cursor_quad = if placeholder || selected_range.is_empty() {
            let start = line_starts.get(cursor_line).copied().unwrap_or(0);
            let cursor_x = if empty.get(cursor_line).copied().unwrap_or(true) {
                px(0.)
            } else {
                lines
                    .get(cursor_line)
                    .map(|line| line.x_for_index(cursor.saturating_sub(start)))
                    .unwrap_or(px(0.))
            };
            let y = bounds.top() - scroll_y + line_h * cursor_line as f32;
            Some(fill(
                Bounds::new(point(bounds.left() + cursor_x, y), size(px(1.5), line_h)),
                theme::accent(),
            ))
        } else {
            None
        };
        PrepaintState {
            line: None,
            lines,
            line_starts,
            cursor: cursor_quad,
            selection: None,
            selections,
            scroll: px(0.),
            scroll_y,
            line_h,
            multiline: true,
            blanks: empty,
        }
    }
}

impl gpui::Render for TextField {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id(self.element_id.clone())
            .flex()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .items_center()
            .overflow_hidden()
            .key_context(if self.picks {
                "TextInput SearchField"
            } else if self.multiline {
                "TextInput NoteField"
            } else {
                "TextInput"
            })
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .text_size(px(12.))
            .text_color(theme::text())
            .when(self.multiline, |field| {
                field.items_start().overflow_hidden()
            })
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::escape))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::accept_suggestion))
            .on_action(cx.listener(Self::pick_up))
            .on_action(cx.listener(Self::pick_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(TextElement { input: cx.entity() })
    }
}

pub fn bind_text_keys(cx: &mut App) {
    use gpui::KeyBinding;
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("TextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("TextInput")),
        KeyBinding::new("ctrl-x", Cut, Some("TextInput")),
        KeyBinding::new("escape", EscapeField, Some("TextInput")),
        KeyBinding::new("enter", Submit, Some("TextInput")),
        KeyBinding::new("tab", AcceptSuggestion, Some("TextInput")),
        KeyBinding::new("up", PickUp, Some("SearchField")),
        KeyBinding::new("down", PickDown, Some("SearchField")),
        KeyBinding::new("up", PickUp, Some("NoteField")),
        KeyBinding::new("down", PickDown, Some("NoteField")),
    ]);
}
