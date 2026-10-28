use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    anchored, deferred, div, img, point, prelude::*, px, svg, Anchor, Animation, AnimationExt,
    Context, IntoElement, MouseButton, ObjectFit, ScrollHandle, Transformation,
};

use crate::ui::cover::cover_art;
use crate::ui::icons::{self, icon};
use crate::ui::import::{self, ImportKind};
use crate::ui::main_window::MainWindow;
use crate::ui::paths;
use crate::ui::session::{self, Track};
use crate::ui::theme;
use crate::ui::ytdlp::{self, JobSnap, Probe};

const MISSING_YTDLP: &str = "yt-dlp was not found. Place yt-dlp.exe in bin next to Muzeeka.";
const TYPE_PAUSE: Duration = Duration::from_millis(320);

struct DownloadJob {
    id: u64,
    url: String,
    probing: bool,
    downloading: bool,
    importing: bool,
    percent: f32,
    status: String,
    error: Option<String>,
    probe: Option<Probe>,
    success: Option<String>,
    success_at: Option<Instant>,
    probe_gen: u64,
    download_gen: u64,
    thumb: Option<PathBuf>,
    job: Option<Arc<Mutex<JobSnap>>>,
}

impl DownloadJob {
    fn new(id: u64, url: String) -> Self {
        Self {
            id,
            url,
            probing: false,
            downloading: false,
            importing: false,
            percent: 0.0,
            status: String::new(),
            error: None,
            probe: None,
            success: None,
            success_at: None,
            probe_gen: 0,
            download_gen: 0,
            thumb: None,
            job: None,
        }
    }

    fn disabled(&self, ytdlp_missing: bool) -> bool {
        self.probing
            || self.downloading
            || self.importing
            || self.success.is_some()
            || ytdlp_missing
    }
}

pub(crate) struct DownloadState {
    jobs: Vec<DownloadJob>,
    next_id: u64,
    ytdlp_ready: Option<bool>,
    last_query: String,
    pending_urls: Vec<String>,
    pending_at: Option<Instant>,
    pub(crate) open: bool,
    pub(crate) close_press: bool,
    history: Vec<SavedDownload>,
    hidden: Vec<String>,
    history_gen: u64,
    listed: Option<ListedCache>,
    scroll: ScrollHandle,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct SavedDownload {
    path: String,
    title: String,
    artist: String,
    #[serde(default)]
    playlist_id: String,
    #[serde(default)]
    at: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HistoryFile {
    #[serde(default)]
    history: Vec<SavedDownload>,
    #[serde(default)]
    hidden: Vec<String>,
}

#[derive(Clone)]
struct ListedDownload {
    path: String,
    title: String,
    artist: String,
    playlist_id: String,
    playlist_name: String,
    cover: Option<PathBuf>,
    present: bool,
}

struct ListedCache {
    epoch: u64,
    gen: u64,
    root: String,
    rows: Vec<ListedDownload>,
}

struct ActiveSnap {
    id: u64,
    title: String,
    status: String,
    error: bool,
    thumb: Option<PathBuf>,
    percent: f32,
    show_bar: bool,
    probing: bool,
    can_cancel: bool,
    can_download: bool,
    can_dismiss: bool,
}

impl Default for DownloadState {
    fn default() -> Self {
        Self {
            jobs: Vec::new(),
            next_id: 1,
            ytdlp_ready: None,
            last_query: String::new(),
            pending_urls: Vec::new(),
            pending_at: None,
            open: false,
            close_press: false,
            history: Vec::new(),
            hidden: Vec::new(),
            history_gen: 0,
            listed: None,
            scroll: ScrollHandle::new(),
        }
    }
}

impl DownloadState {
    pub(crate) fn load() -> Self {
        let mut state = Self::default();
        let (history, hidden) = read_store();
        state.history = history;
        state.hidden = hidden;
        state.history_gen = 1;
        state
    }
}

impl MainWindow {
    pub(crate) fn note_search_cleared(&mut self) {
        self.download.last_query.clear();
        self.download.pending_urls.clear();
        self.download.pending_at = None;
        self.search_pick = 0;
        self.search_pick_query.clear();
    }

    pub(crate) fn note_download_query(&mut self, text: &str, cx: &mut Context<Self>) {
        let trimmed = text.trim().to_string();
        let previous = std::mem::replace(&mut self.download.last_query, trimmed.clone());
        let urls = media_urls(&trimmed);
        if urls.is_empty() {
            self.download.pending_urls.clear();
            self.download.pending_at = None;
            return;
        }
        let grew = !previous.is_empty()
            && trimmed.starts_with(previous.as_str())
            && trimmed.len() > previous.len();
        let shrank = !trimmed.is_empty()
            && previous.starts_with(trimmed.as_str())
            && previous.len() > trimmed.len();
        if grew || shrank {
            self.download.pending_urls = urls;
            self.download.pending_at = Some(Instant::now() + TYPE_PAUSE);
            return;
        }
        self.commit_urls(urls, cx);
    }

    pub(crate) fn tick_download(&mut self, cx: &mut Context<Self>) -> bool {
        let mut dirty = false;
        if self
            .download
            .pending_at
            .is_some_and(|at| Instant::now() >= at)
        {
            let urls = std::mem::take(&mut self.download.pending_urls);
            self.download.pending_at = None;
            self.commit_urls(urls, cx);
            dirty = true;
        }
        let mut finished = Vec::new();
        for job in &mut self.download.jobs {
            if let Some(snap) = &job.job {
                let (percent, status) = job_progress(snap);
                if (percent - job.percent).abs() > 0.4 || status != job.status {
                    job.percent = percent;
                    job.status = status;
                    dirty = true;
                }
            }
            if job
                .success_at
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(1800))
            {
                finished.push(job.id);
            }
        }
        if !finished.is_empty() {
            self.download.jobs.retain(|job| !finished.contains(&job.id));
            dirty = true;
        }
        dirty
    }

    fn commit_urls(&mut self, urls: Vec<String>, cx: &mut Context<Self>) {
        if urls.is_empty() {
            return;
        }
        let current = self.search.read(cx).content.to_string();
        if media_urls(current.trim()) != urls {
            self.download.pending_urls.clear();
            self.download.pending_at = None;
            return;
        }
        self.download.pending_urls.clear();
        self.download.pending_at = None;

        let mut added = false;
        for url in urls.into_iter().rev() {
            added |= self.enqueue_download(url, cx);
        }
        if added {
            self.download.open = true;
        }
        self.download.last_query.clear();
        self.search
            .update(cx, |field, cx| field.replace_text("", cx));
    }

    fn enqueue_download(&mut self, url: String, cx: &mut Context<Self>) -> bool {
        if self
            .download
            .jobs
            .iter()
            .any(|job| job.url == url && job.success.is_none())
        {
            return false;
        }
        if self.download.ytdlp_ready.is_none() {
            self.download.ytdlp_ready = Some(ytdlp::ytdlp_available());
        }
        let id = self.download.next_id;
        self.download.next_id = self.download.next_id.wrapping_add(1).max(1);
        let mut job = DownloadJob::new(id, url);
        if self.download.ytdlp_ready == Some(false) {
            job.error = Some(MISSING_YTDLP.into());
            self.download.jobs.insert(0, job);
            return true;
        }
        self.download.jobs.insert(0, job);
        self.start_probe(id, cx);
        true
    }

    fn job_mut(&mut self, id: u64) -> Option<&mut DownloadJob> {
        self.download.jobs.iter_mut().find(|job| job.id == id)
    }

    fn job_disabled(&self, id: u64) -> bool {
        let missing = self.download.ytdlp_ready == Some(false);
        self.download
            .jobs
            .iter()
            .find(|job| job.id == id)
            .is_none_or(|job| job.disabled(missing))
    }

    fn begin_download(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.job_disabled(id) {
            return;
        }
        let Some(index) = self.download.jobs.iter().position(|job| job.id == id) else {
            return;
        };
        let url = self.download.jobs[index].url.clone();
        let playlist = self.download.jobs[index]
            .probe
            .as_ref()
            .is_some_and(|probe| probe.is_playlist)
            || ytdlp::looks_like_playlist_url(&url);
        let named = if playlist {
            let title = self.download.jobs[index]
                .probe
                .as_ref()
                .map(|probe| probe.title.trim().to_string())
                .filter(|title| !title.is_empty());
            Some(title.unwrap_or_else(|| ytdlp::playlist_name_from_url(&url)))
        } else {
            None
        };
        let configured_id = if named.is_none() {
            ytdlp::read_download_settings().1
        } else {
            None
        };
        let dir = match ytdlp::download_dir(named.as_deref()) {
            Ok(dir) => dir,
            Err(error) => {
                if let Some(job) = self.job_mut(id) {
                    job.error = Some(error);
                }
                cx.notify();
                return;
            }
        };
        let allow_playlist = named.is_some();
        let job = &mut self.download.jobs[index];
        job.download_gen = job.download_gen.wrapping_add(1);
        let gen = job.download_gen;
        job.downloading = true;
        job.importing = false;
        job.percent = 0.0;
        job.status = "Starting download…".into();
        job.error = None;
        job.success = None;
        job.success_at = None;
        let snap = Arc::new(Mutex::new(JobSnap::new()));
        job.job = Some(Arc::clone(&snap));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn({
                    let snap = Arc::clone(&snap);
                    let dir = dir.clone();
                    async move { ytdlp::download(&url, &dir, allow_playlist, snap) }
                })
                .await;
            let next = this
                .update(cx, |view, cx| {
                    let Some(job) = view.job_mut(id) else {
                        return None;
                    };
                    if job.download_gen != gen {
                        return None;
                    }
                    job.job = None;
                    match result {
                        Err(error) => {
                            view.fail_download(id, error, cx);
                            None
                        }
                        Ok(files) => {
                            if let Some(job) = view.job_mut(id) {
                                job.importing = true;
                                job.status = "Adding to library…".into();
                            }
                            cx.notify();
                            Some(files)
                        }
                    }
                })
                .ok()
                .flatten();
            let Some(files) = next else {
                return;
            };
            let count = files.len();
            let scanned = cx
                .background_executor()
                .spawn(async move { import::scan_paths(&files) })
                .await;
            let _ = this.update(cx, |view, cx| {
                let Some(job) = view.download.jobs.iter().find(|job| job.id == id) else {
                    return;
                };
                if job.download_gen != gen {
                    return;
                }
                view.finish_download_import(id, scanned, count, named, configured_id, dir, cx);
            });
        })
        .detach();
    }

    fn finish_download_import(
        &mut self,
        id: u64,
        files: Vec<muzeeka::MusicFile>,
        count: usize,
        named: Option<String>,
        configured_id: Option<String>,
        dir: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if files.is_empty() {
            self.fail_download(id, "No supported audio files found".into(), cx);
            return;
        }
        let thumb = self
            .download
            .jobs
            .iter()
            .find(|job| job.id == id)
            .and_then(|job| job.thumb.clone());
        let kind = ImportKind::Download {
            named: named.clone(),
            configured_id,
            output_dir: dir.to_string_lossy().to_string(),
        };
        let imported = self
            .session
            .update(cx, |session, _cx| session.import_dropped(&files, &kind));
        self.search_hits = None;
        match imported {
            Ok(report) => {
                let playlist_id = report.open_id.clone().unwrap_or_default();
                self.record_downloads(&files, &playlist_id);
                if named.is_some() {
                    if let Some(playlist_id) = report.open_id.clone() {
                        if let Some(thumb) = thumb {
                            if thumb.is_file() {
                                self.session.update(cx, |session, _cx| {
                                    session.set_playlist_cover(&playlist_id, &thumb);
                                });
                            }
                        }
                    }
                }
                if let Some(job) = self.job_mut(id) {
                    job.downloading = false;
                    job.importing = false;
                    job.percent = 100.0;
                    job.status.clear();
                    job.error = None;
                    job.job = None;
                    job.success = Some(if count == 1 {
                        "Added".into()
                    } else {
                        format!("+{count}")
                    });
                    job.success_at = Some(Instant::now());
                }
            }
            Err(error) => {
                self.fail_download(id, error, cx);
                return;
            }
        }
        cx.notify();
    }

    fn fail_download(&mut self, error_id: u64, error: String, cx: &mut Context<Self>) {
        let Some(job) = self.job_mut(error_id) else {
            return;
        };
        job.downloading = false;
        job.importing = false;
        job.job = None;
        job.status.clear();
        if error != "Download cancelled" {
            job.error = Some(error);
        }
        cx.notify();
    }

    fn cancel_download(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(job) = self.job_mut(id) else {
            return;
        };
        job.download_gen = job.download_gen.wrapping_add(1);
        job.downloading = false;
        job.importing = false;
        job.percent = 0.0;
        job.status.clear();
        if let Some(snap) = job.job.take() {
            cancel_process(snap);
        }
        cx.notify();
    }

    fn dismiss_download(&mut self, id: u64, cx: &mut Context<Self>) {
        let importing = self
            .download
            .jobs
            .iter()
            .any(|job| job.id == id && job.importing);
        if importing {
            return;
        }
        if let Some(job) = self.job_mut(id) {
            job.probe_gen = job.probe_gen.wrapping_add(1);
            job.download_gen = job.download_gen.wrapping_add(1);
            if let Some(snap) = job.job.take() {
                cancel_process(snap);
            }
        }
        self.download.jobs.retain(|job| job.id != id);
        cx.notify();
    }

    fn start_probe(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(job) = self.job_mut(id) else {
            return;
        };
        let url = job.url.clone();
        job.probe_gen = job.probe_gen.wrapping_add(1);
        let gen = job.probe_gen;
        job.probing = true;
        job.probe = None;
        job.thumb = None;
        job.error = None;
        job.success = None;
        job.success_at = None;
        cx.spawn(async move |this, cx| {
            let probed = cx
                .background_executor()
                .spawn(async move {
                    let result = ytdlp::probe(&url);
                    let thumb = result.as_ref().ok().and_then(|probe| {
                        probe.thumbnail.as_deref().and_then(ytdlp::fetch_thumbnail)
                    });
                    (result, thumb)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                let Some(job) = view.job_mut(id) else {
                    return;
                };
                if job.probe_gen != gen {
                    return;
                }
                job.probing = false;
                match probed {
                    (Ok(probe), thumb) => {
                        job.probe = Some(probe);
                        job.thumb = thumb;
                        job.error = None;
                    }
                    (Err(error), _) => {
                        job.probe = None;
                        job.thumb = None;
                        job.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn downloads_shown(&mut self, cx: &Context<Self>) -> bool {
        self.inflight_count() > 0 || !self.downloaded_listing(cx).is_empty()
    }

    pub(crate) fn downloads_button(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.download.open;
        let count = self.inflight_count();
        let toggle = self.download_toggle(open, count, cx);
        let panel = open.then(|| self.downloads_panel(cx));
        div()
            .relative()
            .flex_shrink_0()
            .child(toggle)
            .when_some(panel, |wrap, panel| {
                wrap.child(
                    div().absolute().top(px(0.)).left(px(0.)).child(
                        deferred(
                            anchored()
                                .anchor(Anchor::TopLeft)
                                .offset(point(px(0.), px(4. + 32.)))
                                .snap_to_window_with_margin(px(8.))
                                .child(panel),
                        )
                        .priority(2),
                    ),
                )
            })
    }

    fn download_toggle(
        &mut self,
        open: bool,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("downloads-toggle")
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, false, window.mouse_position(), cx);
                }),
            )
            .relative()
            .size(px(32.))
            .rounded_md()
            .cursor_pointer()
            .when(open, |button| button.bg(theme::hover()))
            .hover(|style| style.bg(theme::hover()))
            .child(icon(
                icons::ARROW,
                if count > 0 {
                    theme::accent()
                } else {
                    theme::text_secondary()
                },
                px(16.),
            ))
            .when(count > 0, |button| {
                let label = if count > 9 {
                    "9+".to_string()
                } else {
                    count.to_string()
                };
                button.child(
                    div()
                        .absolute()
                        .top(px(-3.))
                        .right(px(-3.))
                        .min_w(px(14.))
                        .h(px(14.))
                        .px(px(3.))
                        .rounded_full()
                        .bg(theme::accent())
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(9.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::white())
                        .child(label),
                )
            })
            .capture_any_mouse_down(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                if event.button != MouseButton::Left {
                    return;
                }
                cx.stop_propagation();
                let was_open = this.download.open;
                let (open, close_press, arm) =
                    download_button_press(this.download.open, this.download.close_press);
                this.download.open = open;
                this.download.close_press = close_press;
                if arm {
                    this.arm("downloads-toggle", false);
                    return;
                }
                this.armed_ui = None;
                if was_open {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    let closing = std::mem::replace(&mut this.download.close_press, false);
                    if !download_button_release(closing, this.armed("downloads-toggle")) {
                        return;
                    }
                    this.download.open = true;
                    cx.notify();
                }),
            )
    }

    fn downloads_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = self.active_snaps();
        let downloaded = self.downloaded_listing(cx);
        let active_count = active.len();
        let downloaded_count = downloaded.len();
        let scroll = self.download.scroll.clone();
        let bar_count = active.iter().filter(|snap| snap.show_bar).count();
        let body_h = panel_body_height(bar_count, active_count - bar_count, downloaded_count);

        div()
            .id("download-manager")
            .occlude()
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, true, window.mouse_position(), cx);
                }),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                if !this.download.open {
                    return;
                }
                this.download.open = false;
                this.download.close_press = true;
                this.armed_ui = None;
                cx.notify();
            }))
            .w(px(PANEL_W))
            .flex()
            .flex_col()
            .rounded_md()
            .bg(theme::bg_elevated())
            .border_1()
            .border_color(theme::border())
            .shadow_md()
            .overflow_hidden()
            .child(self.download_panel_title(cx))
            .child(
                div()
                    .id("download-manager-scroll")
                    .w_full()
                    .h(px(body_h))
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .when(active_count > 0, |body| {
                        body.child(download_section("In progress", active_count, ""))
                            .children(
                                active
                                    .into_iter()
                                    .map(|snap| self.active_download_row(snap, cx)),
                            )
                            .child(div().h(px(SECTION_GAP)).flex_shrink_0())
                    })
                    .child(self.finished_bar(downloaded_count, cx))
                    .when(downloaded_count == 0, |body| {
                        body.child(
                            div()
                                .h(px(EMPTY_H))
                                .px(px(12.))
                                .flex()
                                .items_center()
                                .text_size(px(11.))
                                .text_color(theme::text_muted())
                                .child("Empty"),
                        )
                    })
                    .children(
                        downloaded
                            .into_iter()
                            .enumerate()
                            .map(|(index, row)| self.downloaded_row(index, row, cx)),
                    ),
            )
            .into_any_element()
    }

    fn download_panel_title(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_shrink_0()
            .h(px(36.))
            .px(px(12.))
            .bg(theme::bg_surface())
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Downloads"),
            )
            .child(self.download_panel_close(cx))
    }

    fn finished_bar(&mut self, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(FINISHED_H))
            .px(px(12.))
            .flex()
            .items_center()
            .justify_between()
            .flex_shrink_0()
            .bg(theme::bg_surface())
            .border_t_1()
            .border_b_1()
            .border_color(theme::border())
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("Finished"),
            )
            .when(count > 0, |bar| bar.child(self.clear_finished_button(cx)))
    }

    fn clear_finished_button(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("download-clear")
            .px(px(4.))
            .py(px(2.))
            .rounded_sm()
            .flex()
            .items_center()
            .text_size(px(11.))
            .text_color(theme::text_secondary())
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
            .child("Clear")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.arm("download-clear", false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    cx.stop_propagation();
                    if !this.armed("download-clear") {
                        return;
                    }
                    this.clear_finished(cx);
                }),
            )
    }

    fn download_panel_close(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("download-manager-close")
            .flex()
            .items_center()
            .justify_center()
            .size(px(22.))
            .rounded_sm()
            .text_size(px(14.))
            .text_color(theme::text_muted())
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
            .child("×")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.arm("download-manager-close", false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if !this.armed("download-manager-close") {
                        return;
                    }
                    this.download.open = false;
                    cx.notify();
                }),
            )
    }

    fn active_download_row(
        &mut self,
        snap: ActiveSnap,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id = snap.id;
        let show_bar = snap.show_bar;
        let percent = snap.percent;
        let status = if show_bar {
            progress_caption(&snap.status)
        } else {
            snap.status.clone()
        };
        div()
            .id(("download-active", id))
            .w_full()
            .h(px(if show_bar { ACTIVE_ROW } else { ACTIVE_IDLE }))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .justify_start()
            .gap(px(6.))
            .px(px(12.))
            .pt(px(6.))
            .pb(px(if show_bar { 0. } else { 6. }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_w(px(0.))
                    .child(manager_thumb(snap.thumb.clone(), snap.probing, id))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(14.))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .truncate()
                                    .child(snap.title.clone()),
                            )
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .text_size(px(10.))
                                    .line_height(px(12.))
                                    .text_color(if snap.error {
                                        gpui::rgb(0xf87171)
                                    } else {
                                        theme::text_secondary()
                                    })
                                    .truncate()
                                    .child(status),
                            ),
                    )
                    .when(snap.can_cancel, |row| {
                        row.child(self.manager_cancel(id, cx))
                    })
                    .when(snap.can_download && !snap.can_cancel, |row| {
                        row.child(self.manager_download(id, cx))
                    })
                    .when(snap.can_dismiss, |row| {
                        row.child(self.manager_dismiss(id, cx))
                    }),
            )
            .when(show_bar, |row| {
                row.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(px(2.))
                        .w_full()
                        .flex_shrink_0()
                        .child(progress_track(percent))
                        .child(
                            div()
                                .h(px(12.))
                                .text_size(px(10.))
                                .line_height(px(12.))
                                .text_color(theme::text_secondary())
                                .child(format!("{percent:.0}%")),
                        ),
                )
            })
            .into_any_element()
    }

    fn manager_cancel(&mut self, id: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let arm_id = format!("mgr-cancel-{id}");
        let release_id = arm_id.clone();
        div()
            .id(("mgr-cancel", id))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .h(px(26.))
            .px(px(8.))
            .rounded_sm()
            .bg(gpui::rgba(0xffffff0f))
            .text_size(px(11.))
            .text_color(theme::text_secondary())
            .cursor_pointer()
            .child("Cancel")
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
                    this.cancel_download(id, cx);
                }),
            )
            .into_any_element()
    }

    fn manager_download(&mut self, id: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let arm_id = format!("mgr-go-{id}");
        let release_id = arm_id.clone();
        div()
            .id(("mgr-go", id))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(px(26.))
            .px(px(8.))
            .rounded_sm()
            .bg(theme::accent())
            .text_size(px(11.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::white())
            .cursor_pointer()
            .child(icon(icons::ARROW, theme::white(), px(12.)))
            .child("Download")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if this.job_disabled(id) {
                        return;
                    }
                    this.arm(arm_id.clone(), false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if !this.armed(&release_id) || this.job_disabled(id) {
                        return;
                    }
                    this.begin_download(id, cx);
                }),
            )
            .into_any_element()
    }

    fn manager_dismiss(&mut self, id: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let arm_id = format!("mgr-x-{id}");
        let release_id = arm_id.clone();
        div()
            .id(("mgr-x", id))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(px(26.))
            .rounded_sm()
            .text_size(px(14.))
            .text_color(theme::text_muted())
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
            .child("×")
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
                    this.dismiss_download(id, cx);
                }),
            )
            .into_any_element()
    }

    fn downloaded_row(
        &mut self,
        index: usize,
        row: ListedDownload,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let present = row.present;
        let path = row.path.clone();
        let playlist_id = row.playlist_id.clone();
        let prefer_play = if playlist_id.is_empty() {
            None
        } else {
            Some(playlist_id.clone())
        };
        let path_play = path.clone();
        let path_go = path.clone();
        let detail = download_detail(&row);
        let cover = row.cover.clone();
        let hide = self.dismiss_finished_button(&path, index, cx);
        div()
            .id(("download-done", index as u64))
            .w_full()
            .h(px(DONE_ROW))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .when(present, |row| row.hover(|style| style.bg(theme::hover())))
            .child(if present {
                let key = format!("dl-{index}");
                self.cover_with_play(&key, 32., cover.as_deref(), cx, move |this, cx| {
                    let prefer = prefer_play.as_deref();
                    let Some(track_id) = this
                        .session
                        .read(cx)
                        .spot_for_path(&path_play)
                        .map(|spot| spot.track_id)
                    else {
                        return;
                    };
                    this.session.update(cx, |session, cx| {
                        session.play_track(track_id, prefer);
                        cx.notify();
                    });
                })
            } else {
                cover_art(cover.as_deref(), 32.)
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(14.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .truncate()
                            .child(row.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .line_height(px(12.))
                            .text_color(if present {
                                theme::text_secondary()
                            } else {
                                theme::text_muted()
                            })
                            .truncate()
                            .child(detail),
                    ),
            )
            .when(present, move |line| {
                let key = format!("dl-{index}");
                line.child(self.go_to_track_button(&key, cx, move |this, window, cx| {
                    let Some(spot) = this.session.read(cx).spot_for_path(&path_go) else {
                        return;
                    };
                    let view = spot.view.clone();
                    let track_id = spot.track_id;
                    this.reveal_in_playlist(track_id, view, window, cx);
                }))
            })
            .child(hide)
            .into_any_element()
    }

    fn dismiss_finished_button(
        &mut self,
        path: &str,
        index: usize,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let path = path.to_string();
        let arm_id = format!("dl-x-{index}");
        let release_id = arm_id.clone();
        div()
            .id(("download-hide", index as u64))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .size(px(22.))
            .rounded_sm()
            .text_size(px(14.))
            .text_color(theme::text_muted())
            .cursor_pointer()
            .hover(|style| style.bg(theme::hover()).text_color(theme::text()))
            .child("×")
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
                    cx.stop_propagation();
                    if !this.armed(&release_id) {
                        return;
                    }
                    this.dismiss_finished(&path, cx);
                }),
            )
            .into_any_element()
    }

    fn inflight_count(&self) -> usize {
        self.download
            .jobs
            .iter()
            .filter(|job| job.probing || job.downloading || job.importing)
            .count()
    }

    fn active_snaps(&self) -> Vec<ActiveSnap> {
        let missing = self.download.ytdlp_ready == Some(false);
        self.download
            .jobs
            .iter()
            .filter(|job| job.success.is_none())
            .map(|job| active_snap(job, missing))
            .collect()
    }

    fn downloaded_listing(&mut self, cx: &Context<Self>) -> Vec<ListedDownload> {
        let epoch = self.session.read(cx).list_epoch();
        let gen = self.download.history_gen;
        let root = ytdlp::effective_download_folder();
        if let Some(cache) = &self.download.listed {
            if cache.epoch == epoch && cache.gen == gen && cache.root == root {
                return cache.rows.clone();
            }
        }
        let hidden: HashSet<String> = self.download.hidden.iter().cloned().collect();
        let rows = {
            let session = self.session.read(cx);
            build_listing(
                session.tracks(),
                session.playlists.as_slice(),
                &self.download.history,
                &hidden,
                &root,
            )
        };
        self.download.listed = Some(ListedCache {
            epoch,
            gen,
            root,
            rows: rows.clone(),
        });
        rows
    }

    fn record_downloads(&mut self, files: &[muzeeka::MusicFile], playlist_id: &str) {
        if files.is_empty() {
            return;
        }
        for file in files.iter().rev() {
            let key = paths::path_key(&file.path);
            self.download.hidden.retain(|row| row != &key);
            remember_history(
                &mut self.download.history,
                SavedDownload {
                    path: file.path.clone(),
                    title: saved_title(file),
                    artist: file.artist.clone().unwrap_or_default(),
                    playlist_id: playlist_id.to_string(),
                    at: now_millis(),
                },
            );
        }
        self.persist_history();
    }

    fn dismiss_finished(&mut self, path: &str, cx: &mut Context<Self>) {
        conceal(&mut self.download.history, &mut self.download.hidden, path);
        self.persist_history();
        self.close_manager_if_idle(cx);
        cx.notify();
    }

    fn clear_finished(&mut self, cx: &mut Context<Self>) {
        let root = ytdlp::effective_download_folder();
        let paths: Vec<String> = {
            let session = self.session.read(cx);
            let mut paths: Vec<String> = session
                .tracks()
                .iter()
                .filter(|track| under_root(&track.path, &root))
                .map(|track| track.path.clone())
                .collect();
            paths.extend(self.download.history.iter().map(|row| row.path.clone()));
            paths
        };
        for path in paths {
            conceal(&mut self.download.history, &mut self.download.hidden, &path);
        }
        self.download.history.clear();
        self.persist_history();
        self.close_manager_if_idle(cx);
        cx.notify();
    }

    fn persist_history(&mut self) {
        self.download.history_gen = self.download.history_gen.wrapping_add(1);
        self.download.listed = None;
        write_store(&self.download.history, &self.download.hidden);
    }

    fn close_manager_if_idle(&mut self, cx: &Context<Self>) {
        if self.inflight_count() == 0 && self.downloaded_listing(cx).is_empty() {
            self.download.open = false;
        }
    }
}

fn download_button_press(open: bool, close_press: bool) -> (bool, bool, bool) {
    if open || close_press {
        (false, true, false)
    } else {
        (false, false, true)
    }
}

fn download_button_release(close_press: bool, armed: bool) -> bool {
    !close_press && armed
}

fn probe_meta(probe: &Probe) -> String {
    let mut parts = Vec::new();
    if let Some(uploader) = probe
        .uploader
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        parts.push(uploader.to_string());
    }
    if probe.is_playlist {
        if let Some(count) = probe.entry_count {
            parts.push(format!("Playlist · {count} tracks"));
        }
    } else if let Some(secs) = probe.duration_secs.filter(|secs| *secs > 0.0) {
        parts.push(session::format_time(secs));
    }
    parts.join(" · ")
}

fn job_progress(job: &Mutex<JobSnap>) -> (f32, String) {
    match job.lock() {
        Ok(guard) => (guard.percent, guard.status.clone()),
        Err(poison) => {
            let guard = poison.into_inner();
            (guard.percent, guard.status.clone())
        }
    }
}

fn cancel_process(snap: Arc<Mutex<JobSnap>>) {
    match snap.lock() {
        Ok(mut guard) => guard.cancel = true,
        Err(poison) => poison.into_inner().cancel = true,
    }
    std::thread::spawn(move || {
        for _ in 0..20 {
            let pid = match snap.lock() {
                Ok(guard) => guard.pid,
                Err(poison) => poison.into_inner().pid,
            };
            if pid != 0 {
                ytdlp::kill_pid(pid);
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

const HISTORY_LIMIT: usize = 300;
const FOLDER_LIMIT: usize = 200;
const HIDDEN_LIMIT: usize = 2000;
const PANEL_W: f32 = 340.;
const ACTIVE_ROW: f32 = 65.;
const ACTIVE_IDLE: f32 = 48.;
const DONE_ROW: f32 = 48.;
const SECTION_H: f32 = 28.;
const FINISHED_H: f32 = 36.;
const SECTION_GAP: f32 = 8.;
const EMPTY_H: f32 = 32.;

fn active_snap(job: &DownloadJob, ytdlp_missing: bool) -> ActiveSnap {
    let title = job
        .probe
        .as_ref()
        .map(|probe| probe.title.trim().to_string())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| job.url.clone());
    let status = if let Some(error) = &job.error {
        error.clone()
    } else if job.probing {
        "Checking…".into()
    } else if job.downloading || job.importing {
        if job.status.is_empty() {
            format!("{:.0}%", job.percent)
        } else {
            job.status.clone()
        }
    } else if let Some(meta) = job
        .probe
        .as_ref()
        .map(probe_meta)
        .filter(|meta| !meta.is_empty())
    {
        meta
    } else {
        "Ready".into()
    };
    ActiveSnap {
        id: job.id,
        title,
        status,
        error: job.error.is_some(),
        thumb: job.thumb.clone(),
        percent: job.percent,
        show_bar: job.downloading || job.importing,
        probing: job.probing,
        can_cancel: job.downloading && !job.importing && job.job.is_some(),
        can_download: !job.disabled(ytdlp_missing),
        can_dismiss: !job.importing && !job.downloading,
    }
}

fn manager_thumb(path: Option<PathBuf>, spin: bool, id: u64) -> gpui::AnyElement {
    if let Some(path) = path {
        return img(path)
            .size(px(36.))
            .flex_shrink_0()
            .rounded(px(4.))
            .object_fit(ObjectFit::Cover)
            .into_any_element();
    }
    div()
        .size(px(36.))
        .flex_shrink_0()
        .rounded(px(4.))
        .bg(theme::bg_surface())
        .flex()
        .items_center()
        .justify_center()
        .when(spin, |slot| {
            slot.child(
                svg()
                    .data(icons::SPINNER)
                    .text_color(theme::accent())
                    .size(px(14.))
                    .with_animation(
                        ("mgr-spin", id),
                        Animation::new(Duration::from_millis(700)).repeat(),
                        |glyph, phase| {
                            glyph.with_transformation(Transformation::rotate(gpui::radians(
                                phase * std::f32::consts::TAU,
                            )))
                        },
                    ),
            )
        })
        .when(!spin, |slot| {
            slot.child(icon(icons::NOTE, theme::text_muted(), px(14.)))
        })
        .into_any_element()
}

fn download_section(title: &str, count: usize, empty: &str) -> gpui::AnyElement {
    div()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(SECTION_H))
                .px(px(12.))
                .flex()
                .items_center()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::text_muted())
                .child(title.to_string()),
        )
        .when(count == 0, |col| {
            col.child(
                div()
                    .h(px(EMPTY_H))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child(empty.to_string()),
            )
        })
        .into_any_element()
}

fn progress_track(percent: f32) -> gpui::AnyElement {
    let fraction = (percent / 100.0).clamp(0.0, 1.0);
    div()
        .w_full()
        .h(px(3.))
        .flex_shrink_0()
        .rounded_full()
        .overflow_hidden()
        .bg(gpui::rgba(0xffffff18))
        .child(
            div()
                .h_full()
                .w(gpui::relative(fraction))
                .rounded_full()
                .bg(theme::accent()),
        )
        .into_any_element()
}

fn progress_caption(status: &str) -> String {
    let status = status.trim();
    if status.is_empty() {
        return "Downloading…".into();
    }
    let Some(index) = status.rfind('%') else {
        return status.to_string();
    };
    let head = status[..index].trim_end();
    let head = head.trim_end_matches(|ch: char| ch.is_ascii_digit() || ch == '.' || ch == ',');
    let head = head.trim().trim_end_matches(['·', '-', ':']).trim();
    if head.is_empty() {
        "Downloading…".into()
    } else {
        head.to_string()
    }
}

fn panel_body_height(active_bar: usize, active_idle: usize, downloaded: usize) -> f32 {
    let active = active_bar + active_idle;
    let active_h = if active == 0 {
        0.
    } else {
        SECTION_H + active_bar as f32 * ACTIVE_ROW + active_idle as f32 * ACTIVE_IDLE + SECTION_GAP
    };
    let done_h = FINISHED_H
        + if downloaded == 0 {
            EMPTY_H
        } else {
            downloaded as f32 * DONE_ROW
        };
    (active_h + done_h).min(420.)
}

fn download_detail(row: &ListedDownload) -> String {
    if !row.present {
        return "Not in the library".into();
    }
    let artist = row.artist.trim();
    let playlist = row.playlist_name.trim();
    match (artist.is_empty(), playlist.is_empty()) {
        (false, false) => format!("{artist} · {playlist}"),
        (false, true) => artist.to_string(),
        (true, false) => playlist.to_string(),
        (true, true) => "All tracks".into(),
    }
}

fn build_listing(
    tracks: &[Track],
    playlists: &[session::Playlist],
    history: &[SavedDownload],
    hidden: &HashSet<String>,
    root: &str,
) -> Vec<ListedDownload> {
    let mut by_key = HashMap::with_capacity(tracks.len());
    for (index, track) in tracks.iter().enumerate() {
        if track.path.is_empty() {
            continue;
        }
        by_key.insert(paths::path_key(&track.path), index);
    }
    let mut first: HashMap<session::TrackId, (String, String)> = HashMap::new();
    for playlist in playlists {
        for id in &playlist.track_ids {
            first
                .entry(*id)
                .or_insert_with(|| (playlist.id.clone(), playlist.name.clone()));
        }
    }
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for saved in history {
        let key = paths::path_key(&saved.path);
        if !seen.insert(key.clone()) || hidden.contains(&key) {
            continue;
        }
        rows.push(listed_from_saved(
            saved, key, tracks, &by_key, playlists, &first,
        ));
    }
    let mut extra = 0usize;
    for track in tracks.iter().rev() {
        if extra >= FOLDER_LIMIT {
            break;
        }
        if track.path.is_empty() || !under_root(&track.path, root) {
            continue;
        }
        let key = paths::path_key(&track.path);
        if hidden.contains(&key) || !seen.insert(key) {
            continue;
        }
        rows.push(listed_from_track(track, playlists, &first));
        extra += 1;
    }
    rows
}

fn listed_from_saved(
    saved: &SavedDownload,
    key: String,
    tracks: &[Track],
    by_key: &HashMap<String, usize>,
    playlists: &[session::Playlist],
    first: &HashMap<session::TrackId, (String, String)>,
) -> ListedDownload {
    let Some(track) = by_key.get(&key).and_then(|index| tracks.get(*index)) else {
        return ListedDownload {
            path: saved.path.clone(),
            title: if saved.title.is_empty() {
                "Track".into()
            } else {
                saved.title.clone()
            },
            artist: saved.artist.clone(),
            playlist_id: saved.playlist_id.clone(),
            playlist_name: String::new(),
            cover: None,
            present: false,
        };
    };
    let (playlist_id, playlist_name) = home_name(track, &saved.playlist_id, playlists, first);
    ListedDownload {
        path: track.path.clone(),
        title: if track.title.is_empty() {
            saved.title.clone()
        } else {
            track.title.clone()
        },
        artist: if track.artist.is_empty() {
            saved.artist.clone()
        } else {
            track.artist.clone()
        },
        playlist_id,
        playlist_name,
        cover: track.cover.clone(),
        present: true,
    }
}

fn home_name(
    track: &Track,
    prefer: &str,
    playlists: &[session::Playlist],
    first: &HashMap<session::TrackId, (String, String)>,
) -> (String, String) {
    if !prefer.is_empty() {
        if let Some(playlist) = playlists
            .iter()
            .find(|playlist| playlist.id == prefer && playlist.track_ids.contains(&track.id))
        {
            return (playlist.id.clone(), playlist.name.clone());
        }
    }
    first
        .get(&track.id)
        .cloned()
        .unwrap_or_else(|| (String::new(), "All tracks".into()))
}

fn saved_title(file: &muzeeka::MusicFile) -> String {
    if let Some(title) = file
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        return title.to_string();
    }
    let stem = std::path::Path::new(&file.file_name)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(file.file_name.as_str());
    if stem.is_empty() {
        "Track".into()
    } else {
        stem.to_string()
    }
}

fn remember_history(history: &mut Vec<SavedDownload>, item: SavedDownload) {
    let key = paths::path_key(&item.path);
    history.retain(|row| paths::path_key(&row.path) != key);
    history.insert(0, item);
    if history.len() > HISTORY_LIMIT {
        history.truncate(HISTORY_LIMIT);
    }
}

fn listed_from_track(
    track: &Track,
    playlists: &[session::Playlist],
    first: &HashMap<session::TrackId, (String, String)>,
) -> ListedDownload {
    let (playlist_id, playlist_name) = home_name(track, "", playlists, first);
    ListedDownload {
        path: track.path.clone(),
        title: if track.title.is_empty() {
            "Track".into()
        } else {
            track.title.clone()
        },
        artist: track.artist.clone(),
        playlist_id,
        playlist_name,
        cover: track.cover.clone(),
        present: true,
    }
}

fn under_root(path: &str, root: &str) -> bool {
    let root = root.trim().trim_end_matches(['/', '\\']);
    if root.is_empty() || path.trim().is_empty() {
        return false;
    }
    let root = paths::path_key(root);
    let path = paths::path_key(path);
    path == root || path.starts_with(&format!("{root}\\")) || path.starts_with(&format!("{root}/"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|dur| dur.as_millis() as u64)
        .unwrap_or(0)
}

fn history_path() -> PathBuf {
    crate::ui::catalog::app_data_dir().join("download-history.json")
}

fn parse_store(text: &str) -> (Vec<SavedDownload>, Vec<String>) {
    let text = text.trim();
    if text.is_empty() {
        return (Vec::new(), Vec::new());
    }
    if let Ok(rows) = serde_json::from_str::<Vec<SavedDownload>>(text) {
        return trim_store(rows, Vec::new());
    }
    if let Ok(file) = serde_json::from_str::<HistoryFile>(text) {
        return trim_store(file.history, file.hidden);
    }
    (Vec::new(), Vec::new())
}

fn trim_store(
    mut history: Vec<SavedDownload>,
    mut hidden: Vec<String>,
) -> (Vec<SavedDownload>, Vec<String>) {
    if history.len() > HISTORY_LIMIT {
        history.truncate(HISTORY_LIMIT);
    }
    if hidden.len() > HIDDEN_LIMIT {
        hidden.truncate(HIDDEN_LIMIT);
    }
    (history, hidden)
}

fn read_store() -> (Vec<SavedDownload>, Vec<String>) {
    let Ok(text) = fs::read_to_string(history_path()) else {
        return (Vec::new(), Vec::new());
    };
    parse_store(&text)
}

fn write_store(history: &[SavedDownload], hidden: &[String]) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = HistoryFile {
        history: history.to_vec(),
        hidden: hidden.to_vec(),
    };
    if let Ok(text) = serde_json::to_string(&file) {
        let _ = fs::write(path, text);
    }
}

fn conceal(history: &mut Vec<SavedDownload>, hidden: &mut Vec<String>, path: &str) {
    let key = paths::path_key(path);
    history.retain(|row| paths::path_key(&row.path) != key);
    hidden.retain(|row| row != &key);
    hidden.insert(0, key);
    if hidden.len() > HIDDEN_LIMIT {
        hidden.truncate(HIDDEN_LIMIT);
    }
}

fn media_urls(text: &str) -> Vec<String> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.is_empty() {
        return Vec::new();
    }
    let mut urls = Vec::with_capacity(parts.len());
    for part in parts {
        let Some(url) = ytdlp::normalize_media_url(part).filter(|url| ytdlp::is_media_url(url))
        else {
            return Vec::new();
        };
        if !link_is_specific(&url) {
            return Vec::new();
        }
        urls.push(url);
    }
    urls
}

fn link_is_specific(url: &str) -> bool {
    let Some(after) = url.split("://").nth(1) else {
        return false;
    };
    let rest = after.split_once('/').map(|(_, rest)| rest).unwrap_or("");
    !rest.trim_matches('/').is_empty() || after.contains('?')
}

#[cfg(test)]
mod tests {
    use super::media_urls;

    #[test]
    fn paste_of_one_link_is_taken() {
        let urls = media_urls("https://www.youtube.com/watch?v=abc123&utm_source=share");
        assert_eq!(urls.len(), 1);
        assert!(urls[0].contains("v=abc123"));
    }

    #[test]
    fn paste_of_several_links_keeps_order() {
        let urls = media_urls(
            "https://youtu.be/abc12345678\nhttps://open.spotify.com/track/0123456789abcDEF",
        );
        assert_eq!(urls.len(), 2);
        assert!(urls[0].contains("youtu.be/abc12345678"));
        assert!(urls[1].contains("/track/0123456789abcDEF"));
    }

    #[test]
    fn search_text_and_a_bare_host_stay_in_the_field() {
        assert!(media_urls("radiohead").is_empty());
        assert!(media_urls("https://youtube.com").is_empty());
        assert!(media_urls("hello https://youtu.be/abc12345678").is_empty());
    }

    #[test]
    fn history_keeps_the_newest_copy_first() {
        let mut history = Vec::new();
        super::remember_history(
            &mut history,
            super::SavedDownload {
                path: r"D:\muz\downloads\a.mp3".into(),
                title: "A".into(),
                artist: String::new(),
                playlist_id: "old".into(),
                at: 1,
            },
        );
        super::remember_history(
            &mut history,
            super::SavedDownload {
                path: r"D:\muz\downloads\b.mp3".into(),
                title: "B".into(),
                artist: String::new(),
                playlist_id: "pl".into(),
                at: 2,
            },
        );
        super::remember_history(
            &mut history,
            super::SavedDownload {
                path: r"d:/muz/downloads/a.mp3".into(),
                title: "A2".into(),
                artist: String::new(),
                playlist_id: "new".into(),
                at: 3,
            },
        );
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].title, "A2");
        assert_eq!(history[0].playlist_id, "new");
        assert_eq!(history[1].title, "B");
    }

    #[test]
    fn second_press_closes_and_does_not_open_again() {
        let (open, close_press, arm) = super::download_button_press(true, false);
        assert!(!open);
        assert!(close_press);
        assert!(!arm);
        assert!(!super::download_button_release(close_press, arm));

        let (open, close_press, arm) = super::download_button_press(open, close_press);
        assert!(!open);
        assert!(close_press);
        assert!(!arm);
        assert!(!super::download_button_release(close_press, true));
    }

    #[test]
    fn press_while_closed_opens_on_release() {
        let (open, close_press, arm) = super::download_button_press(false, false);
        assert!(!open);
        assert!(!close_press);
        assert!(arm);
        assert!(super::download_button_release(close_press, arm));
    }

    #[test]
    fn idle_panel_drops_the_in_progress_block() {
        assert_eq!(
            super::panel_body_height(0, 0, 0),
            super::FINISHED_H + super::EMPTY_H
        );
        assert_eq!(
            super::panel_body_height(1, 0, 0) - super::panel_body_height(0, 0, 0),
            super::SECTION_H + super::ACTIVE_ROW + super::SECTION_GAP
        );
    }

    #[test]
    fn progress_caption_keeps_the_words() {
        assert_eq!(super::progress_caption("Downloading… 45%"), "Downloading…");
        assert_eq!(
            super::progress_caption("Starting download…"),
            "Starting download…"
        );
        assert_eq!(super::progress_caption(""), "Downloading…");
        assert_eq!(super::progress_caption("45%"), "Downloading…");
    }

    fn sample_track(id: u32, title: &str, path: &str) -> crate::ui::session::Track {
        crate::ui::session::Track {
            id: crate::ui::session::TrackId(id),
            title: title.into(),
            artist: "Artist".into(),
            album: String::new(),
            duration: 10.0,
            liked: false,
            path: path.into(),
            cover: None,
            lyrics: None,
        }
    }

    #[test]
    fn download_folder_includes_nested_files_only() {
        assert!(super::under_root(
            r"D:\muz\downloads\nested\a.mp3",
            r"d:/muz/downloads"
        ));
        assert!(!super::under_root(
            r"D:\muz\downloads-extra\a.mp3",
            r"D:\muz\downloads"
        ));
        assert!(!super::under_root("", r"D:\muz\downloads"));
    }

    #[test]
    fn listing_keeps_older_folder_files_and_skips_hidden() {
        let tracks = vec![
            sample_track(1, "Folder", r"D:\muz\downloads\folder.mp3"),
            sample_track(2, "Old", r"D:\muz\downloads\old.mp3"),
            sample_track(3, "Extra", r"D:\muz\downloads-extra\nope.mp3"),
            sample_track(4, "New", r"D:\muz\downloads\new.mp3"),
        ];
        let history = vec![
            super::SavedDownload {
                path: r"D:\muz\kept\gone.mp3".into(),
                title: "Kept".into(),
                artist: String::new(),
                playlist_id: String::new(),
                at: 3,
            },
            super::SavedDownload {
                path: r"D:\muz\downloads\new.mp3".into(),
                title: "New".into(),
                artist: String::new(),
                playlist_id: String::new(),
                at: 2,
            },
            super::SavedDownload {
                path: r"D:\muz\downloads\old.mp3".into(),
                title: "Old".into(),
                artist: String::new(),
                playlist_id: String::new(),
                at: 1,
            },
        ];
        let hidden = std::collections::HashSet::from([crate::ui::paths::path_key(
            r"D:\muz\downloads\old.mp3",
        )]);
        let rows = super::build_listing(&tracks, &[], &history, &hidden, r"D:\muz\downloads");
        let titles: Vec<_> = rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(titles, vec!["Kept", "New", "Folder"]);
        assert!(!rows[0].present);
        assert!(rows[1].present);
        assert!(rows[2].present);
    }

    #[test]
    fn store_reads_an_old_list_and_a_wrapper() {
        let (history, hidden) = super::parse_store(
            r#"[{"path":"D:\\a.mp3","title":"A","artist":"","playlist_id":"p"}]"#,
        );
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].title, "A");
        assert_eq!(history[0].at, 0);
        assert!(hidden.is_empty());

        let (history, hidden) = super::parse_store(
            r#"{"history":[{"path":"D:\\b.mp3","title":"B","artist":"X","playlist_id":"","at":9}],"hidden":["d:\\a.mp3"]}"#,
        );
        assert_eq!(history[0].title, "B");
        assert_eq!(history[0].at, 9);
        assert_eq!(hidden, vec!["d:\\a.mp3"]);

        let (history, hidden) = super::parse_store("{}");
        assert!(history.is_empty());
        assert!(hidden.is_empty());
        let (history, hidden) = super::parse_store("not json");
        assert!(history.is_empty());
        assert!(hidden.is_empty());
    }

    #[test]
    fn conceal_hides_one_path_and_drops_it_from_history() {
        let mut history = vec![super::SavedDownload {
            path: r"D:\muz\downloads\a.mp3".into(),
            title: "A".into(),
            artist: String::new(),
            playlist_id: String::new(),
            at: 1,
        }];
        let mut hidden = Vec::new();
        super::conceal(&mut history, &mut hidden, r"d:/muz/downloads/a.mp3");
        assert!(history.is_empty());
        assert_eq!(hidden.len(), 1);
        super::conceal(&mut history, &mut hidden, r"D:\muz\downloads\a.mp3");
        assert_eq!(hidden.len(), 1);
    }
}
