use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    actions, canvas, div, point, prelude::*, px, size, AnyElement, App, Bounds, Context,
    DispatchPhase, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, Render, ScrollHandle, ScrollWheelEvent, SharedString, SpringAnimation,
    SpringConfig, SpringState, Styled, Task, TitlebarOptions, UniformListScrollHandle, Window,
    WindowBounds, WindowControlArea, WindowHandle, WindowOptions,
};

use crate::ui::properties::{CloseProperties, PropertiesWindow, SaveProperties};
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
        VolumeDown,
        RenamePlaylist
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
    properties: HashMap<String, WindowHandle<PropertiesWindow>>,
    accent_source: Option<(Option<TrackId>, Option<PathBuf>)>,
    accent_epoch: u64,
    revealed_current: bool,
    pub(crate) progress_bounds: Bounds<Pixels>,
    pub(crate) volume_bounds: Bounds<Pixels>,
    pub(crate) drag: Option<SliderDrag>,
    pub(crate) album_width: f32,
    pub(crate) album_resize: Option<(Pixels, f32)>,
    pub(crate) hovered_track: Option<TrackId>,
    pub(crate) hovered_ui: Option<SharedString>,
    pub(crate) hovered_cover: Option<SharedString>,
    pub(crate) pressed_ui: Option<SharedString>,
    pub(crate) armed_ui: Option<SharedString>,
    pub(crate) press_at: Option<gpui::Point<Pixels>>,
    pub(crate) item_drag: Option<crate::ui::item_drag::ItemDrag>,
    pub(crate) track_scroll: UniformListScrollHandle,
    pub(crate) sidebar_scroll: ScrollHandle,
    pub(crate) search_scroll: UniformListScrollHandle,
    menu_scroll: ScrollHandle,
    submenu_scroll: ScrollHandle,
    track_ease: EaseScroll,
    sidebar_ease: EaseScroll,
    search_ease: EaseScroll,
    pub(crate) search_hits: Option<(String, usize, Arc<Vec<TrackId>>)>,
    pub(crate) search_pick: usize,
    pub(crate) search_pick_query: String,
    bar_drag: Option<BarDrag>,
    menu: PopMenu,
    pub(crate) rename_field: gpui::Entity<TextField>,
    rename_wired: bool,
    rename_blur: Option<gpui::Subscription>,
    pub(crate) file_drop: crate::ui::file_drop::FileDropState,
    pub(crate) download: crate::ui::download_ui::DownloadState,
    pending_reveal: Option<(TrackId, u8)>,
    found_flash: Option<(TrackId, Instant)>,
    found_playlists: Vec<String>,
    shown_title: String,
    taskbar_attached: bool,
    pub(crate) listed: Option<crate::ui::track_list::ListedTracks>,
    pub(crate) show_remaining: bool,
    pub(crate) fullscreen: crate::ui::fullscreen::Fullscreen,
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
    Menu,
    Submenu,
}

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

    fn note(
        &mut self,
        handle: &ScrollHandle,
        event: &ScrollWheelEvent,
        reduce_motion: bool,
    ) -> bool {
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
        let settled =
            (self.spring.position - self.target).abs() < 0.5 && self.spring.velocity.abs() < 12.;
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
    pub(crate) fn hover_spring(on: bool) -> SpringAnimation<bool> {
        SpringAnimation::new(SpringConfig::new(580.0, 26.8, 1.0)).to(on)
    }

    pub(crate) fn smooth_config() -> SpringConfig {
        SpringConfig::new(324.0, 36.0, 1.0)
    }

    pub(crate) fn smooth_spring(on: bool) -> SpringAnimation<bool> {
        SpringAnimation::new(Self::smooth_config()).to(on)
    }

    pub(crate) fn smooth_spring_px(offset: f32) -> SpringAnimation<gpui::Pixels> {
        SpringAnimation::new(Self::smooth_config()).to(px(offset))
    }

    pub(crate) fn press_scale(phase: gpui::AnimationPhase) -> f32 {
        1.0 - 0.08 * phase.0.clamp(0.0, 1.0)
    }

    pub(crate) fn arm(&mut self, id: impl Into<SharedString>, shrink: bool) {
        let id = id.into();
        if shrink {
            self.pressed_ui = Some(id.clone());
        }
        self.armed_ui = Some(id);
    }

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
                if !*over && same && this.press_at.is_some() {
                    return;
                }
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

    pub(crate) fn hover_cover_listener(
        id: SharedString,
        cx: &mut Context<Self>,
    ) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        let entity = cx.entity().clone();
        move |over, _, app| {
            entity.update(app, |this, cx| {
                let same = this.hovered_cover.as_ref() == Some(&id);
                if *over {
                    if !same {
                        this.hovered_cover = Some(id.clone());
                        cx.notify();
                    }
                } else if same {
                    this.hovered_cover = None;
                    cx.notify();
                }
            });
        }
    }

    pub fn new(use_ui_font: bool, cx: &mut Context<Self>) -> Self {
        let session = cx.new(|_| Session::open());
        crate::ui::plugins::attach(session.clone(), cx);
        let focus = cx.focus_handle();
        let rename_field = cx.new(|cx| {
            TextField::new(cx, "Playlist name", |_, _, _| {}, |_, _| {})
                .element_id("playlist-rename")
        });
        let host = cx.entity().clone();
        let search = cx.new({
            let session = session.clone();
            let focus = focus.clone();
            let escape_host = host.clone();
            move |cx| {
                let tab_host = escape_host.clone();
                let pick_host = tab_host.clone();
                let mut field = TextField::new(
                    cx,
                    "Search or paste URL",
                    move |text, window, app| {
                        let text = text.to_string();
                        session.update(app, |_session, cx| {
                            cx.notify();
                        });
                        let host = host.clone();
                        window.defer(app, move |_window, app| {
                            host.update(app, |this, cx| {
                                this.note_download_query(&text, cx);
                                this.search_pick = 0;
                                this.search_pick_query = text;
                                cx.notify();
                            });
                        });
                    },
                    move |window, app| {
                        let dismiss = escape_host.update(app, |this, cx| this.on_search_escape(cx));
                        if dismiss {
                            window.focus(&focus, app);
                            escape_host.update(app, |_, cx| cx.notify());
                        }
                    },
                );
                field.set_on_tab(move |text, window, app| {
                    let text = text.to_string();
                    tab_host.update(app, |this, cx| {
                        this.accept_search_suggestion(&text, window, cx)
                    })
                });
                field.set_on_pick(move |text, delta, _window, app| {
                    let text = text.to_string();
                    pick_host.update(app, |this, cx| this.move_search_pick(&text, delta, cx))
                });
                field
            }
        });
        let tick = cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(100))
                .await;
            if this
                .update(cx, |view, cx| {
                    let (playing, can_play, has_prev, has_next) =
                        view.session.read(cx).taskbar_flags();
                    let taskbar_action =
                        crate::ui::taskbar::poll(playing, can_play, has_prev, has_next);
                    if let Some(action) = taskbar_action {
                        view.session.update(cx, |session, cx| {
                            match action {
                                crate::ui::taskbar::Action::Prev => session.prev(),
                                crate::ui::taskbar::Action::Next => session.next(),
                                crate::ui::taskbar::Action::Toggle => {
                                    session.toggle_playback(Vec::new());
                                }
                            }
                            cx.notify();
                        });
                    }
                    let session_dirty = view.session.update(cx, |session, cx| {
                        let dirty = session.tick(0.1);
                        if dirty {
                            cx.notify();
                        }
                        dirty
                    });
                    let drop_dirty = view.tick_file_drop();
                    let download_dirty = view.tick_download(cx);
                    let flash_dirty = view.tick_found_flash(cx.reduce_motion());
                    if session_dirty || drop_dirty || download_dirty || flash_dirty {
                        cx.notify();
                    }
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
            properties: HashMap::new(),
            accent_source: None,
            accent_epoch: 0,
            revealed_current: false,
            progress_bounds: Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.))),
            volume_bounds: Bounds::new(point(px(0.), px(0.)), size(px(0.), px(0.))),
            drag: None,
            album_width: crate::ui::track_list::COL_ALBUM,
            album_resize: None,
            hovered_track: None,
            hovered_ui: None,
            hovered_cover: None,
            pressed_ui: None,
            armed_ui: None,
            press_at: None,
            item_drag: None,
            track_scroll: UniformListScrollHandle::new(),
            sidebar_scroll: ScrollHandle::new(),
            search_scroll: UniformListScrollHandle::new(),
            menu_scroll: ScrollHandle::new(),
            submenu_scroll: ScrollHandle::new(),
            track_ease: EaseScroll::new(),
            sidebar_ease: EaseScroll::new(),
            search_ease: EaseScroll::new(),
            search_hits: None,
            search_pick: 0,
            search_pick_query: String::new(),
            bar_drag: None,
            menu: PopMenu::None,
            rename_field,
            rename_wired: false,
            rename_blur: None,
            file_drop: crate::ui::file_drop::FileDropState::default(),
            download: crate::ui::download_ui::DownloadState::load(),
            pending_reveal: None,
            found_flash: None,
            found_playlists: Vec::new(),
            shown_title: String::new(),
            taskbar_attached: false,
            listed: None,
            show_remaining: session::load_remaining_time(),
            fullscreen: crate::ui::fullscreen::Fullscreen::new(),
            _tick: tick,
        }
    }

    fn scroll_handle(&self, which: Scroller) -> ScrollHandle {
        match which {
            Scroller::Tracks => self.track_scroll.0.borrow().base_handle.clone(),
            Scroller::Sidebar => self.sidebar_scroll.clone(),
            Scroller::Search => self.search_scroll.0.borrow().base_handle.clone(),
            Scroller::Menu => self.menu_scroll.clone(),
            Scroller::Submenu => self.submenu_scroll.clone(),
        }
    }

    fn ease_mut(&mut self, which: Scroller) -> &mut EaseScroll {
        match which {
            Scroller::Tracks => &mut self.track_ease,
            Scroller::Sidebar => &mut self.sidebar_ease,
            Scroller::Search => &mut self.search_ease,
            Scroller::Menu | Scroller::Submenu => unreachable!("context menu scroll is not eased"),
        }
    }

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

    fn reveal_current(&mut self, cx: &mut Context<Self>) {
        if self.revealed_current {
            return;
        }
        if self
            .track_scroll
            .0
            .borrow()
            .base_handle
            .bounds()
            .size
            .height
            <= px(1.)
        {
            return;
        }
        let row = {
            let session = self.session.read(cx);
            session.current.map(|current| {
                session.visible_indexes("").iter().position(|index| {
                    session
                        .track_by_index(*index)
                        .is_some_and(|track| track.id == current)
                })
            })
        };
        self.revealed_current = true;
        if let Some(Some(row)) = row {
            self.track_scroll
                .scroll_to_item(row, gpui::ScrollStrategy::Center);
        }
    }

    fn sync_scrolls_if_idle(&mut self) {
        let tracks = self.scroll_handle(Scroller::Tracks);
        let sidebar = self.scroll_handle(Scroller::Sidebar);
        let search = self.scroll_handle(Scroller::Search);
        self.track_ease.sync_if_idle(&tracks);
        self.sidebar_ease.sync_if_idle(&sidebar);
        self.search_ease.sync_if_idle(&search);
    }

    pub(crate) fn scroll_sidebar_by(&mut self, delta: f32) -> bool {
        let current = self.sidebar_scroll.offset();
        let max = self.sidebar_scroll.max_offset().y.as_f32();
        let y = current.y.as_f32();
        let next = if max > 0. {
            (y + delta).clamp(-max, 0.)
        } else {
            y
        };
        if (next - y).abs() < 0.05 {
            return false;
        }
        self.sidebar_scroll.set_offset(point(current.x, px(next)));
        self.sidebar_ease.snap(next);
        true
    }

    pub(crate) fn release_pointer_gesture(&mut self) -> bool {
        self.drag = None;
        let resized = self.album_resize.take().is_some();
        let released = self.pressed_ui.take().is_some();
        let scrolled = self.bar_drag.take().is_some();
        self.armed_ui = None;
        self.download.close_press = false;
        self.press_at = None;
        resized || released || scrolled
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
            Scroller::Menu => "scrollbar-menu",
            Scroller::Submenu => "scrollbar-submenu",
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
        if !matches!(which, Scroller::Menu | Scroller::Submenu) {
            self.ease_mut(which).snap(offset);
        }
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
        if !matches!(drag.which, Scroller::Menu | Scroller::Submenu) {
            self.ease_mut(drag.which).snap(next);
        }
        cx.notify();
    }

    pub fn open(use_ui_font: bool, cx: &mut App) {
        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        let main = cx
            .open_window(
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
        let main_id = main.window_id();
        cx.on_window_closed(move |cx, closed| {
            if closed != main_id {
                return;
            }
            cx.defer(|cx| {
                crate::ui::plugins::shutdown(cx);
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
                cx.quit();
            });
        })
        .detach();
        cx.activate(true);
    }

    fn sync_accent(&mut self, cx: &mut Context<Self>) {
        let (track_id, cover) = {
            let session = self.session.read(cx);
            let track = session.current_track();
            (
                track.map(|track| track.id),
                track.and_then(|track| track.cover.clone()),
            )
        };
        let source = (track_id, cover.clone());
        if self.accent_source.as_ref() == Some(&source) {
            return;
        }
        self.accent_source = Some(source);
        self.accent_epoch = self.accent_epoch.wrapping_add(1);
        let epoch = self.accent_epoch;
        let Some(path) = cover.filter(|path| path.is_file()) else {
            self.publish_accent(theme::DEFAULT_ACCENT, cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            let rgb = cx
                .background_executor()
                .spawn(async move { crate::ui::accent::from_cover(&path) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.accent_epoch != epoch {
                    return;
                }
                view.publish_accent(rgb.unwrap_or(theme::DEFAULT_ACCENT), cx);
            });
        })
        .detach();
    }

    fn publish_accent(&self, rgb: u32, cx: &mut Context<Self>) {
        if theme::set_accent_rgb(rgb) {
            cx.refresh_windows();
        }
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

    fn rename_open_playlist(
        &mut self,
        _: &RenamePlaylist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = match &self.session.read(cx).view {
            session::LibraryView::Playlist(id) => id.clone(),
            session::LibraryView::All | session::LibraryView::Liked => return,
        };
        if matches!(&self.menu, PopMenu::Rename { id: editing } if editing == &id) {
            return;
        }
        self.begin_rename(id, window, cx);
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

    pub(crate) fn open_properties(
        &mut self,
        ids: Vec<TrackId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = {
            let session = self.session.read(cx);
            ids.iter()
                .filter_map(|id| session.track(*id).map(|track| track.path.clone()))
                .collect::<Vec<_>>()
        };
        if paths.is_empty() {
            return;
        }
        let key = crate::ui::properties::properties_key(&paths);
        self.menu = PopMenu::None;
        self.armed_ui = None;
        cx.notify();
        let session = self.session.clone();
        let use_ui_font = self.use_ui_font;
        let existing = self.properties.get(&key).copied();
        cx.defer_in(window, move |this, _window, cx| {
            if let Some(handle) = PropertiesWindow::open(session, ids, use_ui_font, existing, cx) {
                this.properties.insert(key, handle);
            }
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
        self.retire_item_drag_if_finished(cx);
        self.tick_drop_motion(window, cx.reduce_motion());
        self.sync_scrolls_if_idle();
        self.tick_playlist_autoscroll(window);
        self.reveal_current(cx);
        self.reveal_queued(cx);
        self.pump_found_flash(window, cx.reduce_motion());
        self.warm_scrollbars(window, cx);
        self.sync_accent(cx);
        self.sync_title(window, cx);
        self.attach_taskbar(window);
        self.wire_rename(window, cx);
        let session = self.session.clone();
        let main_ui: Vec<AnyElement> = if self.fullscreen_covered() {
            Vec::new()
        } else {
            vec![
                self.header(window, cx).into_any_element(),
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .px(px(8.))
                    .pb(px(102.))
                    .child(self.sidebar(cx))
                    .child(self.track_list(cx))
                    .into_any_element(),
                self.transport(cx).into_any_element(),
                self.search_suggestions(window, cx).into_any_element(),
            ]
        };
        let fullscreen = self.fullscreen_layer(window, cx);
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
            .child(crate::ui::item_drag::drag_cursor_layer(cx.entity().clone()))
            .key_context("Muzeeka")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_play))
            .on_action(cx.listener(Self::next_track))
            .on_action(cx.listener(Self::prev_track))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::volume_up))
            .on_action(cx.listener(Self::volume_down))
            .on_action(cx.listener(Self::rename_open_playlist))
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
                if event.keystroke.key == "f11" {
                    window.toggle_fullscreen();
                    return;
                }
                if event.keystroke.key == "escape" && this.download.open {
                    this.download.open = false;
                    cx.notify();
                    return;
                }
                if event.keystroke.key == "escape" && this.menu.is_open() {
                    this.close_menu(window, cx);
                } else if event.keystroke.key == "escape" && this.fullscreen.is_open() {
                    this.close_fullscreen(cx);
                }
            }))
            .on_modifiers_changed(cx.listener(
                |this, event: &gpui::ModifiersChangedEvent, window, cx| {
                    if event.modifiers.alt {
                        this.hand_held_tracks_out(window, cx);
                    }
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    let changed = this.release_pointer_gesture();
                    let leftover = this.item_drag.take().is_some();
                    if leftover {
                        this.set_reorder_gap(None);
                    }
                    if changed || leftover {
                        cx.notify();
                    }
                }),
            )
            .on_drag_move::<gpui::ExternalPaths>(cx.listener(
                |this, event: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                    this.note_file_drag(event, cx);
                },
            ))
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, false, window.mouse_position(), cx);
                }),
            )
            .on_file_drop_exit(cx.listener(|this, _: &gpui::FileDropEvent, _, cx| {
                this.clear_file_drag(cx);
            }))
            .on_drag_move::<crate::ui::item_drag::TrackDrag>(cx.listener(
                |this, event: &gpui::DragMoveEvent<crate::ui::item_drag::TrackDrag>, window, cx| {
                    this.note_track_drag(event, window, cx);
                },
            ))
            .on_drag_move::<crate::ui::item_drag::PlaylistDrag>(cx.listener(
                |this,
                 event: &gpui::DragMoveEvent<crate::ui::item_drag::PlaylistDrag>,
                 window,
                 cx| {
                    this.note_playlist_drag(event, window, cx);
                },
            ))
            .on_drop(
                cx.listener(|this, drag: &crate::ui::item_drag::TrackDrag, _, cx| {
                    this.finish_track_drag(drag, cx);
                }),
            )
            .on_drop(
                cx.listener(|this, drag: &crate::ui::item_drag::PlaylistDrag, _, cx| {
                    this.finish_playlist_drag(drag, cx);
                }),
            )
            .children(main_ui)
            .children(fullscreen)
            .child(self.drop_overlay(window, cx))
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
        self.commit_rename_if_editing(window, cx);
        self.armed_ui = None;
        self.reset_menu_scroll();
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
        self.commit_rename_if_editing(window, cx);
        self.armed_ui = None;
        self.reset_menu_scroll();
        self.menu = PopMenu::Playlist { id, at };
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(crate) fn renaming_id(&self) -> Option<&str> {
        match &self.menu {
            PopMenu::Rename { id } => Some(id.as_str()),
            _ => None,
        }
    }

    pub(crate) fn dismiss_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_rename_if_editing(window, cx);
        self.close_menu(window, cx);
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

    pub(crate) fn reveal_in_playlist(
        &mut self,
        id: TrackId,
        view: crate::ui::session::LibraryView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session.update(cx, |session, cx| {
            session.view = view;
            cx.notify();
        });
        let y = self.track_scroll.0.borrow().base_handle.offset().y.as_f32();
        self.track_ease.snap(y);
        self.pending_reveal = Some((id, 3));
        self.found_flash = None;
        self.found_playlists.clear();
        self.download.open = false;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn reveal_queued(&mut self, cx: &mut Context<Self>) {
        let Some((id, left)) = self.pending_reveal else {
            return;
        };
        if self
            .track_scroll
            .0
            .borrow()
            .base_handle
            .bounds()
            .size
            .height
            <= px(1.)
        {
            return;
        }
        let row = {
            let session = self.session.read(cx);
            session.visible_indexes("").iter().position(|index| {
                session
                    .track_by_index(*index)
                    .is_some_and(|track| track.id == id)
            })
        };
        if let Some(row) = row {
            self.track_scroll
                .scroll_to_item(row, gpui::ScrollStrategy::Center);
            if self.found_flash.is_none() {
                self.found_playlists = self.session.read(cx).playlists_to_flash(id);
                self.found_flash = Some((id, Instant::now()));
            }
        }
        self.pending_reveal = if row.is_some() && left > 1 {
            Some((id, left - 1))
        } else {
            None
        };
    }

    pub(crate) fn found_flash_amount(&self, id: TrackId, reduce_motion: bool) -> f32 {
        let Some((found, started)) = self.found_flash else {
            return 0.0;
        };
        if found != id {
            return 0.0;
        }
        flash_amount(started.elapsed().as_millis(), reduce_motion)
    }

    pub(crate) fn found_playlist_flash_amount(&self, id: &str, reduce_motion: bool) -> f32 {
        let Some((_, started)) = self.found_flash else {
            return 0.0;
        };
        if !self.found_playlists.iter().any(|playlist| playlist == id) {
            return 0.0;
        }
        flash_amount(started.elapsed().as_millis(), reduce_motion)
    }

    fn pump_found_flash(&mut self, window: &mut Window, reduce_motion: bool) {
        let Some((_, started)) = self.found_flash else {
            return;
        };
        if flash_done(started.elapsed().as_millis(), reduce_motion) {
            self.found_flash = None;
            self.found_playlists.clear();
            return;
        }
        if !reduce_motion {
            window.request_animation_frame();
        }
    }

    fn tick_found_flash(&mut self, reduce_motion: bool) -> bool {
        let Some((_, started)) = self.found_flash else {
            return false;
        };
        if flash_done(started.elapsed().as_millis(), reduce_motion) {
            self.found_flash = None;
            self.found_playlists.clear();
            true
        } else {
            false
        }
    }

    fn on_search_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.download.open {
            self.download.open = false;
            cx.notify();
            return false;
        }
        let empty = self.search.read(cx).content.is_empty();
        if !empty {
            self.search.update(cx, |field, cx| field.clear(cx));
            self.note_search_cleared();
            cx.notify();
            return false;
        }
        true
    }

    fn sync_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.session.read(cx).window_title();
        if self.shown_title == title {
            return;
        }
        self.shown_title = title.clone();
        window.set_window_title(&title);
    }

    fn attach_taskbar(&mut self, window: &Window) {
        if self.taskbar_attached {
            return;
        }
        self.taskbar_attached = crate::ui::taskbar::attach(window);
    }

    fn wire_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let handle = self.rename_field.read(cx).focus.clone();
        self.rename_blur = Some(cx.on_blur(&handle, window, |this, window, cx| {
            this.commit_rename_if_editing(window, cx);
        }));
    }

    fn commit_rename_if_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.menu, PopMenu::Rename { .. }) {
            return;
        }
        let name = self.rename_field.read(cx).content.to_string();
        self.commit_rename(name, window, cx);
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

    pub(crate) fn begin_rename(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let name = self
            .session
            .read(cx)
            .playlists
            .iter()
            .find(|playlist| playlist.id == id)
            .map(|playlist| playlist.name.clone())
            .unwrap_or_default();
        self.rename_field.update(cx, |field, cx| {
            field.set_text(name, window, cx);
            field.select_all_text(cx);
        });
        self.menu = PopMenu::Rename { id };
        let handle = self.rename_field.read(cx).focus.clone();
        let entity = cx.entity().clone();
        window.on_next_frame(move |window, cx| {
            entity.update(cx, |this, cx| {
                if matches!(this.menu, PopMenu::Rename { .. }) {
                    window.focus(&handle, cx);
                }
            });
        });
        cx.notify();
    }

    fn reset_menu_scroll(&self) {
        let top = point(px(0.), px(0.));
        self.menu_scroll.set_offset(top);
        self.submenu_scroll.set_offset(top);
    }

    fn warm_menu_bar(&self, which: Scroller, window: &mut Window, cx: &mut Context<Self>) {
        if self.scroll_handle(which).bounds().size.height > px(1.) {
            return;
        }
        let entity = cx.entity().clone();
        window.on_next_frame(move |_window, cx| {
            entity.update(cx, |_this, cx| cx.notify());
        });
    }

    fn menu_frame(
        &mut self,
        id: &'static str,
        which: Scroller,
        min_w: f32,
        items: Vec<AnyElement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = items.len();
        let handle = self.scroll_handle(which);
        let mut list = div()
            .id(id)
            .flex()
            .flex_col()
            .min_w(px(min_w))
            .max_w(px(320.))
            .max_h(px(crate::ui::menu::max_height() - 2.))
            .overflow_y_scroll()
            .p(px(4.))
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, true, window.mouse_position(), cx);
                }),
            )
            .track_scroll(&handle);
        if count > crate::ui::menu::VISIBLE_ROWS {
            list = list.pr(px(14.));
        }
        self.warm_menu_bar(which, window, cx);
        div()
            .relative()
            .occlude()
            .rounded(px(6.))
            .border_1()
            .border_color(theme::border())
            .shadow_md()
            .bg(theme::bg_surface())
            .child(list.children(items))
            .child(self.scrollbar(which, cx))
            .into_any_element()
    }

    fn menu_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !self.menu.is_open() {
            return div().into_any_element();
        }
        if matches!(self.menu, PopMenu::None | PopMenu::Rename { .. }) {
            return div().into_any_element();
        }
        let at = match self.menu {
            PopMenu::Track { at, .. } | PopMenu::Playlist { at, .. } => at,
            PopMenu::None | PopMenu::Rename { .. } => return div().into_any_element(),
        };
        let panel = match self.menu.clone() {
            PopMenu::Track { .. } => self.track_menu(window, cx),
            PopMenu::Playlist { .. } => self.playlist_menu(window, cx),
            PopMenu::None | PopMenu::Rename { .. } => return div().into_any_element(),
        };
        div()
            .absolute()
            .size_full()
            .child(
                crate::ui::menu::backdrop("context-backdrop")
                    .on_drop(
                        cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                            this.receive_file_drop(paths, false, window.mouse_position(), cx);
                        }),
                    )
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

    fn track_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
            self.session
                .read(cx)
                .playlists
                .iter()
                .any(|playlist| &playlist.id == playlist_id && playlist.track_ids.contains(&id))
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
                move |this, window, cx| {
                    this.open_properties(vec![id], window, cx);
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
                    this.submenu_scroll.set_offset(point(px(0.), px(0.)));
                    this.menu = PopMenu::Track {
                        id,
                        at,
                        submenu: !submenu,
                    };
                    cx.notify();
                },
            ),
            self.bind_menu(
                "track-like",
                crate::ui::menu::Tone::Normal,
                like_label,
                cx,
                move |this, window, cx| {
                    this.session.update(cx, |session, cx| {
                        session.toggle_like(id);
                        cx.notify();
                    });
                    this.close_menu(window, cx);
                },
            ),
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
        let main = self.menu_frame("track-menu", Scroller::Menu, 220., items, window, cx);
        if !submenu {
            return main;
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
            .child(div().mt(px(68.)).child(self.menu_frame(
                "track-playlist-menu",
                Scroller::Submenu,
                180.,
                sub,
                window,
                cx,
            )))
            .into_any_element()
    }

    fn playlist_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let PopMenu::Playlist { id, .. } = self.menu.clone() else {
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
                move |this, window, cx| this.begin_rename(id.clone(), window, cx)
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
        self.menu_frame("playlist-menu", Scroller::Menu, 200., items, window, cx)
    }

    fn header(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.session.read(cx).playback_error.clone();
        let show_downloads = self.downloads_shown(cx);
        div()
            .flex()
            .items_center()
            .h(px(50.))
            .px(px(8.))
            .gap(px(8.))
            .window_control_area(WindowControlArea::Drag)
            .child(self.search_bar(window, cx))
            .when(show_downloads, |bar| bar.child(self.downloads_button(cx)))
            .child(self.playback_error_slot(error, cx))
            .child(self.header_buttons(window, cx))
    }

    fn playback_error_slot(
        &self,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .items_center()
            .overflow_hidden()
            .when_some(error, |slot, error| {
                slot.child(
                    div()
                        .id("playback-error")
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_size(px(12.))
                        .text_color(theme::danger())
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .cursor_pointer()
                        .child(error)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.session.update(cx, |session, cx| {
                                    session.clear_playback_error();
                                    cx.notify();
                                });
                            }),
                        ),
                )
            })
            .into_any_element()
    }
}

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

pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    crate::ui::text_field::bind_text_keys(cx);
    cx.bind_keys([
        KeyBinding::new("space", TogglePlay, Some("!TextInput")),
        KeyBinding::new("ctrl-right", NextTrack, Some("!TextInput")),
        KeyBinding::new("ctrl-left", PrevTrack, Some("!TextInput")),
        KeyBinding::new("ctrl-f", FocusSearch, None),
        KeyBinding::new("tab", FocusSearch, Some("!TextInput")),
        KeyBinding::new("ctrl-up", VolumeUp, Some("!TextInput")),
        KeyBinding::new("ctrl-down", VolumeDown, Some("!TextInput")),
        KeyBinding::new("f2", RenamePlaylist, Some("!TextInput")),
        KeyBinding::new("ctrl-s", SaveProperties, None),
        KeyBinding::new("escape", CloseProperties, Some("Properties && !TextInput")),
    ]);
}

const FLASH_RISE_MS: u128 = 170;
const FLASH_FALL_MS: u128 = 170;
const FLASH_REST_MS: u128 = 60;
const FLASH_COUNT: u128 = 4;
const FLASH_CYCLE_MS: u128 = FLASH_RISE_MS + FLASH_FALL_MS + FLASH_REST_MS;
const FLASH_TOTAL_MS: u128 = FLASH_CYCLE_MS * FLASH_COUNT;
const FLASH_STILL_MS: u128 = 800;

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn flash_done(elapsed_ms: u128, reduce_motion: bool) -> bool {
    elapsed_ms
        >= if reduce_motion {
            FLASH_STILL_MS
        } else {
            FLASH_TOTAL_MS
        }
}

fn flash_amount(elapsed_ms: u128, reduce_motion: bool) -> f32 {
    if reduce_motion {
        return if elapsed_ms < FLASH_STILL_MS {
            1.0
        } else {
            0.0
        };
    }
    if elapsed_ms >= FLASH_TOTAL_MS {
        return 0.0;
    }
    let into = elapsed_ms % FLASH_CYCLE_MS;
    if into < FLASH_RISE_MS {
        smoothstep(into as f32 / FLASH_RISE_MS as f32)
    } else if into < FLASH_RISE_MS + FLASH_FALL_MS {
        let t = (into - FLASH_RISE_MS) as f32 / FLASH_FALL_MS as f32;
        1.0 - smoothstep(t)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::{
        flash_amount, FLASH_CYCLE_MS, FLASH_FALL_MS, FLASH_RISE_MS, FLASH_STILL_MS, FLASH_TOTAL_MS,
    };

    #[test]
    fn found_track_blinks_four_times_then_stays_off() {
        assert_eq!(flash_amount(0, false), 0.0);
        let early = flash_amount(FLASH_RISE_MS / 4, false);
        assert!(early < 0.25, "{early}");
        let mid_rise = flash_amount(FLASH_RISE_MS / 2, false);
        assert!((mid_rise - 0.5).abs() < 0.02, "{mid_rise}");
        assert!((flash_amount(FLASH_RISE_MS, false) - 1.0).abs() < 1e-4);
        let mid_fall = flash_amount(FLASH_RISE_MS + FLASH_FALL_MS / 2, false);
        assert!((mid_fall - 0.5).abs() < 0.02, "{mid_fall}");
        assert_eq!(flash_amount(FLASH_RISE_MS + FLASH_FALL_MS, false), 0.0);
        assert_eq!(flash_amount(FLASH_CYCLE_MS - 1, false), 0.0);
        assert!((flash_amount(FLASH_CYCLE_MS + FLASH_RISE_MS, false) - 1.0).abs() < 1e-4);
        assert!((flash_amount(FLASH_CYCLE_MS * 3 + FLASH_RISE_MS, false) - 1.0).abs() < 1e-4);
        assert_eq!(flash_amount(FLASH_TOTAL_MS, false), 0.0);
        assert_eq!(flash_amount(5000, false), 0.0);

        let mut prev = 0.0;
        let mut max_step = 0.0_f32;
        for ms in 0..=FLASH_TOTAL_MS {
            let next = flash_amount(ms, false);
            max_step = max_step.max((next - prev).abs());
            prev = next;
        }
        assert!(max_step < 0.02, "{max_step}");

        assert_eq!(flash_amount(0, true), 1.0);
        assert_eq!(flash_amount(FLASH_STILL_MS - 1, true), 1.0);
        assert_eq!(flash_amount(FLASH_STILL_MS, true), 0.0);
    }
}
