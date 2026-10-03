use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    actions, canvas, div, point, prelude::*, px, size, AnyElement, App, Bounds, Context,
    DispatchPhase, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Render, ScrollHandle, ScrollWheelEvent, SharedString, SpringAnimation,
    SpringConfig, SpringState, Styled, Task, TitlebarOptions, UniformListScrollHandle, Window,
    WindowBounds, WindowControlArea, WindowHandle, WindowOptions,
};

use crate::ui::session::{self, Session, TrackId};
use crate::ui::settings::SettingsWindow;
use crate::ui::text_field::TextField;
use crate::ui::theme;

actions!(
    muzeeka,
    [
        TogglePlay,
        NextTrack,
        PrevTrack,
        FocusSearch,
        VolumeUp,
        VolumeDown
    ]
);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SliderDrag {
    Progress,
    Volume,
}

pub struct MainWindow {
    pub(crate) session: gpui::Entity<Session>,
    pub(crate) search: gpui::Entity<TextField>,
    pub(crate) focus: FocusHandle,
    pub(crate) use_ui_font: bool,
    settings_window: Option<WindowHandle<SettingsWindow>>,
    pub(crate) progress_bounds: Bounds<Pixels>,
    pub(crate) volume_bounds: Bounds<Pixels>,
    pub(crate) drag: Option<SliderDrag>,
    /// Album column width in the track table. Dragged from the header grip.
    pub(crate) album_width: f32,
    /// Pointer x and album width when the header grip was pressed.
    pub(crate) album_resize: Option<(Pixels, f32)>,
    /// Track row currently under the pointer. Drives the list hover fade.
    pub(crate) hovered_track: Option<TrackId>,
    /// Sidebar row, transport button, or slider currently under the pointer.
    pub(crate) hovered_ui: Option<SharedString>,
    /// Transport button held down. Drives the press shrink.
    pub(crate) pressed_ui: Option<SharedString>,
    /// Control that received the mouse down. Its action runs on mouse up
    /// only while this still matches, and only if the pointer is still over it.
    /// GPUI on_click is not used: a pending click clears on_hover for the hold.
    pub(crate) armed_ui: Option<SharedString>,
    /// Track list scroll. Wheel notches ease toward a target instead of jumping.
    pub(crate) track_scroll: UniformListScrollHandle,
    /// Playlist list in the sidebar. Same eased wheel as the track list.
    pub(crate) sidebar_scroll: ScrollHandle,
    /// Search results plate. Same eased wheel as the track list.
    pub(crate) search_scroll: UniformListScrollHandle,
    track_ease: EaseScroll,
    sidebar_ease: EaseScroll,
    search_ease: EaseScroll,
    /// Query, library size, and hit ids. Reused while the text is unchanged
    /// so a wheel frame does not scan the library again.
    search_hits: Option<(String, usize, Arc<Vec<TrackId>>)>,
    bar_drag: Option<BarDrag>,
    menu: PopMenu,
    rename_field: gpui::Entity<TextField>,
    rename_wired: bool,
    _tick: Task<()>,
}

#[derive(Clone)]
enum PopMenu {
    None,
    Track {
        id: TrackId,
        at: gpui::Point<Pixels>,
        submenu: bool,
    },
    Playlist {
        id: String,
        at: gpui::Point<Pixels>,
    },
    Rename {
        id: String,
        at: gpui::Point<Pixels>,
    },
    Properties {
        id: TrackId,
    },
}

impl PopMenu {
    fn is_open(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scroller {
    Tracks,
    Sidebar,
    Search,
}

/// Critically damped follow of a scroll offset. Not the hover spring.
struct EaseScroll {
    spring: SpringState,
    target: f32,
    animating: bool,
    stamp: Instant,
}

impl EaseScroll {
    fn new() -> Self {
        Self {
            spring: SpringState::default(),
            target: 0.,
            animating: false,
            stamp: Instant::now(),
        }
    }

    /// GPUI already applied the wheel delta. Put the offset back and ease to it.
    /// Returns true when a frame should be queued.
    fn note(&mut self, handle: &ScrollHandle, event: &ScrollWheelEvent, reduce_motion: bool) -> bool {
        let jumped = handle.offset().y.as_f32();
        if event.delta.precise() || reduce_motion {
            self.snap(jumped);
            return false;
        }
        let applied = jumped - self.spring.position;
        if applied == 0. {
            return false;
        }
        self.target += applied;
        self.clamp(handle);
        handle.set_offset(point(px(0.), px(self.spring.position)));
        true
    }

    fn sync_if_idle(&mut self, handle: &ScrollHandle) {
        if self.animating {
            return;
        }
        self.snap(handle.offset().y.as_f32());
    }

    fn snap(&mut self, y: f32) {
        self.spring.position = y;
        self.spring.velocity = 0.;
        self.target = y;
        self.animating = false;
    }

    fn clamp(&mut self, handle: &ScrollHandle) {
        let max = handle.max_offset().y.as_f32();
        if max > 0. {
            self.target = self.target.clamp(-max, 0.);
        }
    }

    /// Steps the spring. Returns true while it still needs another frame.
    fn step(&mut self, handle: &ScrollHandle) -> bool {
        let now = Instant::now();
        let dt = now
            .saturating_duration_since(self.stamp)
            .as_secs_f32()
            .clamp(0.001, 0.05);
        self.stamp = now;
        self.clamp(handle);
        let stiffness = 220.;
        let config = SpringConfig::new(stiffness, 2. * stiffness.sqrt(), 1.);
        self.spring = config.step(self.spring, self.target, dt);
        let max = handle.max_offset().y.as_f32();
        if max > 0. {
            if self.spring.position < -max {
                self.spring.position = -max;
                self.spring.velocity = 0.;
            } else if self.spring.position > 0. {
                self.spring.position = 0.;
                self.spring.velocity = 0.;
            }
        }
        handle.set_offset(point(px(0.), px(self.spring.position)));
        let settled = (self.spring.position - self.target).abs() < 0.5
            && self.spring.velocity.abs() < 12.;
        if settled {
            self.snap(self.target);
            handle.set_offset(point(px(0.), px(self.target)));
            false
        } else {
            true
        }
    }
}

#[derive(Clone, Copy)]
struct BarDrag {
    which: Scroller,
    origin_y: f32,
    origin_offset: f32,
    travel: f32,
    max: f32,
}

/// Thumb height, top, and drag travel inside the scroll viewport.
fn thumb_metrics(offset: f32, viewport: f32, max: f32) -> Option<(f32, f32, f32)> {
    if max <= 1. || viewport <= 1. {
        return None;
    }
    let content = viewport + max;
    let inset = 2.;
    let usable = viewport - inset * 2.;
    if usable < 1. {
        return None;
    }
    let min_thumb = usable.min(24.);
    let thumb_h = (usable * viewport / content).clamp(min_thumb, usable);
    let travel = usable - thumb_h;
    let fraction = (-offset / max).clamp(0., 1.);
    Some((thumb_h, inset + fraction * travel, travel))
}

impl MainWindow {
    /// Same spring as the track-list hover. Stiffness, damping, mass.
    pub(crate) fn hover_spring(on: bool) -> SpringAnimation<bool> {
        SpringAnimation::new(SpringConfig::new(580.0, 26.8, 1.0)).to(on)
    }

    /// Pressed transport buttons draw at 92% so the shrink stays small.
    pub(crate) fn press_scale(phase: gpui::AnimationPhase) -> f32 {
        1.0 - 0.08 * phase.0.clamp(0.0, 1.0)
    }

    /// Arm a control on mouse down. `shrink` also starts the press scale.
    pub(crate) fn arm(&mut self, id: impl Into<SharedString>, shrink: bool) {
        let id = id.into();
        if shrink {
            self.pressed_ui = Some(id.clone());
        }
        self.armed_ui = Some(id);
    }

    /// True when this control is the one the pointer went down on.
    pub(crate) fn armed(&self, id: &str) -> bool {
        self.armed_ui.as_deref() == Some(id)
    }

    pub(crate) fn hover_listener(
        id: SharedString,
        cx: &mut Context<Self>,
    ) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        let entity = cx.entity().clone();
        move |over, _, app| {
            entity.update(app, |this, cx| {
                let same = this.hovered_ui.as_ref() == Some(&id);
                if *over {
                    if !same {
                        this.hovered_ui = Some(id.clone());
                        cx.notify();
                    }
                } else if same {
                    this.hovered_ui = None;
                    cx.notify();
                }
            });
        }
    }

    pub fn new(use_ui_font: bool, cx: &mut Context<Self>) -> Self {
        let session = cx.new(|_| Session::open());
        let focus = cx.focus_handle();
        let rename_field = cx.new(|cx| {
            TextField::new(cx, "Playlist name", |_, _, _| {}, |_, _| {}).element_id("playlist-rename")
        });
        let search = cx.new({
            let session = session.clone();
            let focus = focus.clone();
            move |cx| {
                TextField::new(
                    cx,
                    "Search or paste URL",
                    move |_text, _window, app| {
                        session.update(app, |session, cx| {
                            session.notice = None;
                            cx.notify();
                        });
                    },
                    move |window, app| {
                        window.focus(&focus, app);
                    },
                )
            }
        });
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if this
                .update(cx, |view, cx| {
                    view.session.update(cx, |session, cx| {
                        if session.tick(0.1) {
                            cx.notify();
                        }
                    });
                })
                .is_err()
            {
                break;
            }
        });

        Self {
            session,
            search,
            focus,
            use_ui_font,
            settings_window: None,
            progress_bounds: Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.))),
            volume_bounds: Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.))),
            drag: None,
            album_width: crate::ui::track_list::COL_ALBUM,
            album_resize: None,
            hovered_track: None,
            hovered_ui: None,
            pressed_ui: None,
            armed_ui: None,
            track_scroll: UniformListScrollHandle::new(),
            sidebar_scroll: ScrollHandle::new(),
            search_scroll: UniformListScrollHandle::new(),
            track_ease: EaseScroll::new(),
            sidebar_ease: EaseScroll::new(),
            search_ease: EaseScroll::new(),
            search_hits: None,
            bar_drag: None,
            menu: PopMenu::None,
            rename_field,
            rename_wired: false,
            _tick: tick,
        }
    }

    fn scroll_handle(&self, which: Scroller) -> ScrollHandle {
        match which {
            Scroller::Tracks => self.track_scroll.0.borrow().base_handle.clone(),
            Scroller::Sidebar => self.sidebar_scroll.clone(),
            Scroller::Search => self.search_scroll.0.borrow().base_handle.clone(),
        }
    }

    fn ease_mut(&mut self, which: Scroller) -> &mut EaseScroll {
        match which {
            Scroller::Tracks => &mut self.track_ease,
            Scroller::Sidebar => &mut self.sidebar_ease,
            Scroller::Search => &mut self.search_ease,
        }
    }

    /// Hit ids for the open query. The scan runs when the text or the library
    /// size changes, and the plate then scrolls back to the top.
    pub(crate) fn cached_search_ids(
        &mut self,
        query: &str,
        cx: &mut Context<Self>,
    ) -> Arc<Vec<TrackId>> {
        let track_count = self.session.read(cx).track_count();
        if let Some((cached_query, cached_count, hits)) = &self.search_hits {
            if cached_query == query && *cached_count == track_count {
                return Arc::clone(hits);
            }
        }
        let hits = Arc::new(self.session.read(cx).search_ids(query));
        self.search_hits = Some((query.to_string(), track_count, Arc::clone(&hits)));
        self.search_ease.snap(0.);
        self.search_scroll
            .0
            .borrow()
            .base_handle
            .set_offset(point(px(0.), px(0.)));
        hits
    }

    /// One extra frame after the results list mounts, so the thumb can read
    /// a real viewport. Skipped once that viewport exists.
    pub(crate) fn warm_search_scrollbar(&self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self
            .search_scroll
            .0
            .borrow()
            .base_handle
            .bounds()
            .size
            .height
            <= px(1.);
        if !pending {
            return;
        }
        let entity = cx.entity().clone();
        window.on_next_frame(move |_window, cx| {
            entity.update(cx, |_this, cx| cx.notify());
        });
    }

    /// Mouse-wheel lines jump by a full row. The scroller applies that jump
    /// first; this puts the offset back and eases toward the same place.
    pub(crate) fn note_scroll(
        &mut self,
        which: Scroller,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let handle = self.scroll_handle(which);
        let kick = self
            .ease_mut(which)
            .note(&handle, event, cx.reduce_motion());
        if kick {
            cx.notify();
            self.kick_scroll(which, window, cx);
        }
    }

    /// The thumb is built from the previous layout. Ask for one more frame
    /// until that layout has a real viewport, so the bar shows without a hover.
    fn warm_scrollbars(&self, window: &mut Window, cx: &mut Context<Self>) {
        let tracks_pending = self
            .track_scroll
            .0
            .borrow()
            .base_handle
            .bounds()
            .size
            .height
            <= px(1.);
        let sidebar_pending = !self.session.read(cx).playlists.is_empty()
            && self.sidebar_scroll.bounds().size.height <= px(1.);
        if !tracks_pending && !sidebar_pending {
            return;
        }
        let entity = cx.entity().clone();
        window.on_next_frame(move |_window, cx| {
            entity.update(cx, |_this, cx| cx.notify());
        });
    }

    fn sync_scrolls_if_idle(&mut self) {
        let tracks = self.scroll_handle(Scroller::Tracks);
        let sidebar = self.scroll_handle(Scroller::Sidebar);
        let search = self.scroll_handle(Scroller::Search);
        self.track_ease.sync_if_idle(&tracks);
        self.sidebar_ease.sync_if_idle(&sidebar);
        self.search_ease.sync_if_idle(&search);
    }

    fn kick_scroll(&mut self, which: Scroller, window: &mut Window, cx: &mut Context<Self>) {
        if self.ease_mut(which).animating {
            return;
        }
        {
            let ease = self.ease_mut(which);
            ease.animating = true;
            ease.stamp = Instant::now();
        }
        self.queue_scroll(which, window, cx);
    }

    fn queue_scroll(&mut self, which: Scroller, window: &mut Window, cx: &mut Context<Self>) {
        let entity = cx.entity().clone();
        window.on_next_frame(move |window, cx| {
            entity.update(cx, |this, cx| this.advance_scroll(which, window, cx));
        });
    }

    fn advance_scroll(&mut self, which: Scroller, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.scroll_handle(which);
        let still = {
            let ease = self.ease_mut(which);
            if !ease.animating {
                return;
            }
            ease.step(&handle)
        };
        if still {
            self.queue_scroll(which, window, cx);
        }
        cx.notify();
    }

    /// Overlay thumb. GPUI does not paint scrollbars. Hidden when nothing overflows.
    pub(crate) fn scrollbar(&self, which: Scroller, cx: &mut Context<Self>) -> AnyElement {
        let handle = self.scroll_handle(which);
        let Some((thumb_h, thumb_y, _)) = thumb_metrics(
            handle.offset().y.as_f32(),
            handle.bounds().size.height.as_f32(),
            handle.max_offset().y.as_f32(),
        ) else {
            return div().absolute().size(px(0.)).into_any_element();
        };
        let id = match which {
            Scroller::Tracks => "scrollbar-tracks",
            Scroller::Sidebar => "scrollbar-sidebar",
            Scroller::Search => "scrollbar-search",
        };
        let hot = self.bar_drag.is_some_and(|drag| drag.which == which)
            || self.hovered_ui.as_deref() == Some(id);
        let color = if hot {
            theme::scrollbar_thumb_hover()
        } else {
            theme::scrollbar_thumb()
        };
        div()
            .absolute()
            .top(px(0.))
            .right(px(2.))
            .bottom(px(0.))
            .w(px(6.))
            .child(
                div()
                    .id(id)
                    .absolute()
                    .top(px(thumb_y))
                    .left(px(0.))
                    .w(px(6.))
                    .h(px(thumb_h))
                    .rounded_full()
                    .bg(color)
                    .on_hover(Self::hover_listener(SharedString::from(id), cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            this.begin_bar_drag(which, event.position.y, cx);
                        }),
                    ),
            )
            .into_any_element()
    }

    fn begin_bar_drag(&mut self, which: Scroller, y: Pixels, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let handle = self.scroll_handle(which);
        let offset = handle.offset().y.as_f32();
        let max = handle.max_offset().y.as_f32();
        let Some((_, _, travel)) = thumb_metrics(offset, handle.bounds().size.height.as_f32(), max)
        else {
            return;
        };
        if travel <= 0. {
            return;
        }
        self.ease_mut(which).snap(offset);
        self.bar_drag = Some(BarDrag {
            which,
            origin_y: y.as_f32(),
            origin_offset: offset,
            travel,
            max,
        });
        cx.notify();
    }

    fn drag_bar(&mut self, y: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.bar_drag else {
            return;
        };
        let delta = y.as_f32() - drag.origin_y;
        let next = (drag.origin_offset - delta / drag.travel * drag.max).clamp(-drag.max, 0.);
        let handle = self.scroll_handle(drag.which);
        handle.set_offset(point(px(0.), px(next)));
        self.ease_mut(drag.which).snap(next);
        cx.notify();
    }

    pub fn open(use_ui_font: bool, cx: &mut App) {
        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("muzeeka".into()),
                    appears_transparent: true,
                    traffic_light_position: None,
                }),
                window_min_size: Some(size(px(860.), px(520.))),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| Self::new(use_ui_font, cx));
                let focus = view.read(cx).focus.clone();
                window.focus(&focus, cx);
                view
            },
        )
        .expect("open main window");
        cx.activate(true);
    }

    fn toggle_play(&mut self, _: &TogglePlay, _: &mut Window, cx: &mut Context<Self>) {
        self.with_queue(cx, |session, queue| session.toggle_playback(queue));
    }

    fn next_track(&mut self, _: &NextTrack, _: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| {
            session.next();
            cx.notify();
        });
    }

    fn prev_track(&mut self, _: &PrevTrack, _: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| {
            session.prev();
            cx.notify();
        });
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        let focus = self.search.read(cx).focus.clone();
        focus.focus(window, cx);
    }

    fn volume_up(&mut self, _: &VolumeUp, _: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| {
            session.set_volume(session.volume + 0.05);
            cx.notify();
        });
    }

    fn volume_down(&mut self, _: &VolumeDown, _: &mut Window, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| {
            session.set_volume(session.volume - 0.05);
            cx.notify();
        });
    }

    fn with_queue(
        &mut self,
        cx: &mut Context<Self>,
        apply: impl FnOnce(&mut Session, Vec<crate::ui::session::TrackId>),
    ) {
        let queue = self
            .session
            .read(cx)
            .visible_tracks("")
            .into_iter()
            .map(|track| track.id)
            .collect();
        self.session.update(cx, |session, cx| {
            apply(session, queue);
            cx.notify();
        });
    }

    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let use_ui_font = self.use_ui_font;
        let existing = self.settings_window;
        cx.defer_in(window, move |this, _window, cx| {
            this.settings_window = SettingsWindow::open(session, use_ui_font, existing, cx);
        });
    }
}

impl Focusable for MainWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_scrolls_if_idle();
        self.warm_scrollbars(window, cx);
        self.wire_rename(cx);
        let session = self.session.clone();
        div()
            .id("muzeeka-root")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::bg_deep())
            .text_color(theme::text())
            .when(self.use_ui_font, |root| {
                root.font_family(theme::FONT_FAMILY)
            })
            .child(alt_wheel_layer(session))
            .key_context("Muzeeka")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_play))
            .on_action(cx.listener(Self::next_track))
            .on_action(cx.listener(Self::prev_track))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::volume_up))
            .on_action(cx.listener(Self::volume_down))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if !event.dragging() {
                    return;
                }
                if this.album_resize.is_some() {
                    this.resize_album(event.position.x, cx);
                } else if this.bar_drag.is_some() {
                    this.drag_bar(event.position.y, cx);
                } else {
                    this.drag_to(event.position.x, cx);
                }
            }))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.menu.is_open() {
                    this.close_menu(window, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.drag = None;
                    let resized = this.album_resize.take().is_some();
                    let released = this.pressed_ui.take().is_some();
                    let scrolled = this.bar_drag.take().is_some();
                    this.armed_ui = None;
                    if resized || released || scrolled {
                        cx.notify();
                    }
                }),
            )
            .child(self.header(window, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .px(px(8.))
                    .pb(px(102.))
                    .child(self.sidebar(cx))
                    .child(self.track_list(cx)),
            )
            .child(self.transport(cx))
            .child(self.search_suggestions(window, cx))
            .child(self.menu_layer(window, cx))
    }
}

impl MainWindow {
    pub(crate) fn open_track_menu(
        &mut self,
        id: TrackId,
        at: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.armed_ui = None;
        self.menu = PopMenu::Track {
            id,
            at,
            submenu: false,
        };
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(crate) fn open_playlist_menu(
        &mut self,
        id: String,
        at: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.armed_ui = None;
        self.menu = PopMenu::Playlist { id, at };
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.menu.is_open() {
            return;
        }
        self.menu = PopMenu::None;
        self.armed_ui = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn wire_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename_wired {
            return;
        }
        self.rename_wired = true;
        let submit_host = cx.entity().clone();
        let escape_host = submit_host.clone();
        self.rename_field.update(cx, |field, _| {
            field.set_on_submit(move |text, window, app| {
                let text = text.to_string();
                submit_host.update(app, |this, cx| this.commit_rename(text, window, cx));
            });
            field.set_on_escape(move |window, app| {
                escape_host.update(app, |this, cx| this.close_menu(window, cx));
            });
        });
    }

    fn commit_rename(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let PopMenu::Rename { id, .. } = &self.menu else {
            return;
        };
        let id = id.clone();
        self.menu = PopMenu::None;
        self.armed_ui = None;
        self.session.update(cx, |session, cx| {
            session.rename_playlist(&id, &name);
            cx.notify();
        });
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn begin_rename(
        &mut self,
        id: String,
        at: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = self
            .session
            .read(cx)
            .playlists
            .iter()
            .find(|playlist| playlist.id == id)
            .map(|playlist| playlist.name.clone())
            .unwrap_or_default();
        self.rename_field
            .update(cx, |field, cx| field.set_text(name, window, cx));
        self.menu = PopMenu::Rename { id, at };
        let handle = self.rename_field.read(cx).focus.clone();
        window.focus(&handle, cx);
        cx.notify();
    }

    fn menu_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        if !self.menu.is_open() {
            return div().into_any_element();
        }
        if let PopMenu::Properties { id } = self.menu {
            return self.properties_layer(id, cx);
        }
        let at = match self.menu {
            PopMenu::Track { at, .. } | PopMenu::Playlist { at, .. } | PopMenu::Rename { at, .. } => at,
            PopMenu::None | PopMenu::Properties { .. } => return div().into_any_element(),
        };
        let panel = match self.menu.clone() {
            PopMenu::Track { .. } => self.track_menu(cx),
            PopMenu::Playlist { .. } => self.playlist_menu(cx),
            PopMenu::Rename { .. } => self.rename_menu(cx),
            PopMenu::None | PopMenu::Properties { .. } => return div().into_any_element(),
        };
        div()
            .absolute()
            .size_full()
            .child(
                crate::ui::menu::backdrop("context-backdrop")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                    ),
            )
            .child(crate::ui::menu::at(at, panel))
            .into_any_element()
    }

    fn bind_menu(
        &mut self,
        id: impl Into<SharedString>,
        tone: crate::ui::menu::Tone,
        label: impl Into<SharedString>,
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let id = id.into();
        let arm = id.clone();
        let up = id.clone();
        let disabled = tone == crate::ui::menu::Tone::Disabled;
        let row = crate::ui::menu::row(div().id(id), tone).child(label.into());
        if disabled {
            return row.into_any_element();
        }
        row.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.arm(arm.clone(), false);
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _: &gpui::MouseUpEvent, window, cx| {
                if this.armed(up.as_ref()) {
                    action(this, window, cx);
                }
            }),
        )
        .into_any_element()
    }

    fn track_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let PopMenu::Track { id, at, submenu } = self.menu.clone() else {
            return div().into_any_element();
        };
        let (path, liked, open_playlist, targets) = {
            let session = self.session.read(cx);
            let track = session.track(id);
            let open_playlist = match &session.view {
                crate::ui::session::LibraryView::Playlist(playlist) => Some(playlist.clone()),
                _ => None,
            };
            let targets = session
                .playlists
                .iter()
                .filter(|playlist| Some(&playlist.id) != open_playlist.as_ref())
                .map(|playlist| (playlist.id.clone(), playlist.name.clone()))
                .collect::<Vec<_>>();
            (
                track.map(|track| track.path.clone()).unwrap_or_default(),
                track.map(|track| track.liked).unwrap_or(false),
                open_playlist,
                targets,
            )
        };
        let in_playlist = open_playlist.as_ref().is_some_and(|playlist_id| {
            self.session.read(cx).playlists.iter().any(|playlist| {
                &playlist.id == playlist_id && playlist.track_ids.contains(&id)
            })
        });
        let like_label = if liked {
            "Remove from Liked"
        } else {
            "Add to Liked"
        };
        let mut items = vec![
            self.bind_menu(
                "track-find",
                crate::ui::menu::Tone::Normal,
                "Найти на диске",
                cx,
                move |this, window, cx| {
                    if let Err(error) = crate::ui::menu::reveal_on_disk(&path) {
                        this.session.update(cx, |session, cx| {
                            session.push_notice(error);
                            cx.notify();
                        });
                    }
                    this.close_menu(window, cx);
                },
            ),
            self.bind_menu(
                "track-properties",
                crate::ui::menu::Tone::Normal,
                "Properties",
                cx,
                move |this, _, cx| {
                    this.menu = PopMenu::Properties { id };
                    cx.notify();
                },
            ),
            self.bind_menu(
                "track-add",
                if targets.is_empty() {
                    crate::ui::menu::Tone::Disabled
                } else {
                    crate::ui::menu::Tone::Normal
                },
                "Добавить в плейлист ›",
                cx,
                move |this, _, cx| {
                    this.menu = PopMenu::Track {
                        id,
                        at,
                        submenu: !submenu,
                    };
                    cx.notify();
                },
            ),
            self.bind_menu("track-like", crate::ui::menu::Tone::Normal, like_label, cx, move |this, window, cx| {
                this.session.update(cx, |session, cx| {
                    session.toggle_like(id);
                    cx.notify();
                });
                this.close_menu(window, cx);
            }),
        ];
        if let Some(playlist_id) = open_playlist.clone().filter(|_| in_playlist) {
            items.push(self.bind_menu(
                "track-delete",
                crate::ui::menu::Tone::Danger,
                "Delete",
                cx,
                move |this, window, cx| {
                    this.session.update(cx, |session, cx| {
                        session.remove_from_playlist(&playlist_id, id);
                        cx.notify();
                    });
                    this.close_menu(window, cx);
                },
            ));
        }
        let main = crate::ui::menu::panel("track-menu")
            .min_w(px(220.))
            .children(items);
        if !submenu {
            return main.into_any_element();
        }
        let sub = targets
            .into_iter()
            .map(|(playlist_id, name)| {
                let label = name.clone();
                self.bind_menu(
                    format!("track-add|{playlist_id}"),
                    crate::ui::menu::Tone::Normal,
                    label,
                    cx,
                    move |this, window, cx| {
                        this.session.update(cx, |session, cx| {
                            if !session.add_to_playlist(&playlist_id, id) {
                                session.push_notice(format!("Already in {name}"));
                            }
                            cx.notify();
                        });
                        this.close_menu(window, cx);
                    },
                )
            })
            .collect::<Vec<_>>();
        div()
            .flex()
            .items_start()
            .gap(px(4.))
            .child(main)
            .child(
                div().mt(px(68.)).child(
                    crate::ui::menu::panel("track-playlist-menu")
                        .min_w(px(180.))
                        .children(sub),
                ),
            )
            .into_any_element()
    }

    fn playlist_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let PopMenu::Playlist { id, at } = self.menu.clone() else {
            return div().into_any_element();
        };
        let (mix_mode, has_cover) = {
            let session = self.session.read(cx);
            let playlist = session.playlists.iter().find(|playlist| playlist.id == id);
            (
                playlist.map(|playlist| playlist.mix_mode).unwrap_or(false),
                playlist
                    .and_then(|playlist| playlist.cover_path.as_ref())
                    .is_some(),
            )
        };
        let mix_label = if mix_mode {
            "Disable Mix mode"
        } else {
            "Enable Mix mode"
        };
        let mut items = vec![
            self.bind_menu(
                "playlist-mix",
                crate::ui::menu::Tone::Normal,
                mix_label,
                cx,
                {
                    let id = id.clone();
                    move |this, window, cx| {
                        this.session.update(cx, |session, cx| {
                            let next = session
                                .playlists
                                .iter()
                                .find(|playlist| playlist.id == id)
                                .map(|playlist| !playlist.mix_mode)
                                .unwrap_or(true);
                            session.set_mix_mode(&id, next);
                            cx.notify();
                        });
                        this.close_menu(window, cx);
                    }
                },
            ),
            self.bind_menu(
                "playlist-cover",
                crate::ui::menu::Tone::Normal,
                "Set cover image",
                cx,
                {
                    let id = id.clone();
                    move |this, window, cx| {
                        this.close_menu(window, cx);
                        match crate::ui::menu::pick_image() {
                            Ok(Some(path)) => {
                                this.session.update(cx, |session, cx| {
                                    session.set_playlist_cover(&id, &path);
                                    cx.notify();
                                });
                            }
                            Ok(None) => {}
                            Err(error) => {
                                this.session.update(cx, |session, cx| {
                                    session.push_notice(error);
                                    cx.notify();
                                });
                            }
                        }
                    }
                },
            ),
        ];
        if has_cover {
            items.push(self.bind_menu(
                "playlist-clear-cover",
                crate::ui::menu::Tone::Normal,
                "Remove cover image",
                cx,
                {
                    let id = id.clone();
                    move |this, window, cx| {
                        this.session.update(cx, |session, cx| {
                            session.clear_playlist_cover(&id);
                            cx.notify();
                        });
                        this.close_menu(window, cx);
                    }
                },
            ));
        }
        items.push(self.bind_menu(
            "playlist-rename",
            crate::ui::menu::Tone::Normal,
            "Rename",
            cx,
            {
                let id = id.clone();
                move |this, window, cx| this.begin_rename(id.clone(), at, window, cx)
            },
        ));
        items.push(self.bind_menu(
            "playlist-delete",
            crate::ui::menu::Tone::Danger,
            "Delete",
            cx,
            {
                let id = id.clone();
                move |this, window, cx| {
                    this.session.update(cx, |session, cx| {
                        session.delete_playlist(&id);
                        cx.notify();
                    });
                    this.close_menu(window, cx);
                }
            },
        ));
        crate::ui::menu::panel("playlist-menu")
            .min_w(px(200.))
            .children(items)
            .into_any_element()
    }

    fn rename_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        crate::ui::menu::panel("rename-menu")
            .min_w(px(220.))
            .child(
                div()
                    .h(px(28.))
                    .px(px(8.))
                    .mb(px(4.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .bg(theme::bg_elevated())
                    .child(self.rename_field.clone()),
            )
            .child(self.bind_menu(
                "rename-save",
                crate::ui::menu::Tone::Accent,
                "Save",
                cx,
                |this, window, cx| {
                    let name = this.rename_field.read(cx).content.to_string();
                    this.commit_rename(name, window, cx);
                },
            ))
            .into_any_element()
    }

    fn properties_layer(&mut self, id: TrackId, cx: &mut Context<Self>) -> AnyElement {
        let Some(track) = self.session.read(cx).track(id).cloned() else {
            return div().into_any_element();
        };
        let file = crate::ui::menu::file_on_disk(&track.path).to_string();
        let cue = track.path.contains("#cue:");
        div()
            .absolute()
            .size_full()
            .child(
                crate::ui::menu::backdrop("properties-backdrop")
                    .bg(gpui::rgba(0x00000099))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top(px(72.))
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .id("properties-card")
                            .occlude()
                            .w(px(420.))
                            .p(px(16.))
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .rounded(px(6.))
                            .bg(theme::bg_elevated())
                            .border_1()
                            .border_color(theme::border())
                            .shadow_md()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Properties"),
                            )
                            .child(property_line("Title", &track.title))
                            .child(property_line("Artist", &track.artist))
                            .child(property_line("Album", &track.album))
                            .child(property_line(
                                "Duration",
                                &session::format_time(track.duration),
                            ))
                            .child(property_line("File", &file))
                            .when(cue, |card| card.child(property_line("Cue", &track.path)))
                            .child(self.bind_menu(
                                "properties-close",
                                crate::ui::menu::Tone::Normal,
                                "Close",
                                cx,
                                |this, window, cx| this.close_menu(window, cx),
                            )),
                    ),
            )
            .into_any_element()
    }

    fn header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .h(px(50.))
            .px(px(8.))
            .gap(px(8.))
            .window_control_area(WindowControlArea::Drag)
            .child(self.search_bar(window, cx))
            .child(div().flex_1().h_full())
            .child(self.header_buttons(window, cx))
    }
}

/// Alt+wheel changes volume by 1% anywhere over this window.
/// Registered in capture, before list and slider wheels, so those do not also move.
pub(crate) fn alt_wheel_layer(session: gpui::Entity<Session>) -> impl IntoElement {
    canvas(
        |_, _, _| session,
        |_, session, window, _cx| {
            window.on_mouse_event(move |event: &ScrollWheelEvent, phase, _, cx| {
                if phase != DispatchPhase::Capture || !event.alt {
                    return;
                }
                let delta_y = match event.delta {
                    gpui::ScrollDelta::Lines(point) => point.y,
                    gpui::ScrollDelta::Pixels(point) => point.y.as_f32(),
                };
                let Some(step) = session::volume_wheel_step(delta_y) else {
                    return;
                };
                cx.stop_propagation();
                session.update(cx, |session, cx| {
                    session.set_volume(session.volume + step);
                    cx.notify();
                });
            });
        },
    )
    .absolute()
    .size_full()
}

fn property_line(label: &str, value: &str) -> AnyElement {
    let value = if value.is_empty() {
        "—".to_string()
    } else {
        value.to_string()
    };
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme::text_muted())
                .child(label.to_string()),
        )
        .child(div().text_size(px(13.)).child(value))
        .into_any_element()
}

pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    crate::ui::text_field::bind_text_keys(cx);
    cx.bind_keys([
        KeyBinding::new("space", TogglePlay, Some("!TextInput")),
        KeyBinding::new("ctrl-right", NextTrack, Some("!TextInput")),
        KeyBinding::new("ctrl-left", PrevTrack, Some("!TextInput")),
        KeyBinding::new("ctrl-f", FocusSearch, None),
        KeyBinding::new("ctrl-up", VolumeUp, Some("!TextInput")),
        KeyBinding::new("ctrl-down", VolumeDown, Some("!TextInput")),
    ]);
}
