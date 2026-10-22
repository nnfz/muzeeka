//! Media-link panel under the search field.
//!
//! A pasted URL is probed after 320ms. Download runs yt-dlp, then the finished
//! files go through the same library import as a file drop.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    div, img, prelude::*, px, svg, Animation, AnimationExt, Context, IntoElement, MouseButton,
    ObjectFit, Stateful, Transformation,
};

use crate::ui::icons::icon;
use crate::ui::import::{self, ImportKind};
use crate::ui::main_window::MainWindow;
use crate::ui::session;
use crate::ui::theme;
use crate::ui::ytdlp::{self, JobSnap, Probe};

const MISSING_YTDLP: &str = "yt-dlp was not found. Place yt-dlp.exe in bin next to Muzeeka.";

const NOTE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#efefef" d="M9 17.5a2.5 2.5 0 1 1-2-2.45V5.2l12-2v10.15a2.5 2.5 0 1 1-2-2.45V6.1l-8 1.35V17.5z"/></svg>"##;
const ARROW: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#efefef" d="M11 3h2v9.2l3.3-3.3 1.4 1.4L12 16.1 6.3 10.3l1.4-1.4L11 12.2V3zM5 19h14v2H5z"/></svg>"##;
const SPINNER: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="2.2" r="1.7" fill="#efefef"/><circle cx="12.4" cy="4.6" r="1.45" fill="#efefef" opacity=".8"/><circle cx="13.8" cy="8.8" r="1.2" fill="#efefef" opacity=".55"/><circle cx="11.2" cy="12.4" r="1.05" fill="#efefef" opacity=".35"/><circle cx="7.2" cy="13.6" r="0.9" fill="#efefef" opacity=".2"/></svg>"##;

pub(crate) struct DownloadState {
    pub(crate) probing: bool,
    pub(crate) downloading: bool,
    pub(crate) importing: bool,
    percent: f32,
    status: String,
    error: Option<String>,
    probe: Option<Probe>,
    success: Option<String>,
    success_at: Option<Instant>,
    ytdlp_ready: Option<bool>,
    probe_gen: u64,
    download_gen: u64,
    probe_after: Option<Instant>,
    pending_query: String,
    probed_query: String,
    thumb: Option<PathBuf>,
    job: Option<Arc<Mutex<JobSnap>>>,
}

impl Default for DownloadState {
    fn default() -> Self {
        Self {
            probing: false,
            downloading: false,
            importing: false,
            percent: 0.0,
            status: String::new(),
            error: None,
            probe: None,
            success: None,
            success_at: None,
            ytdlp_ready: None,
            probe_gen: 0,
            download_gen: 0,
            probe_after: None,
            pending_query: String::new(),
            probed_query: String::new(),
            thumb: None,
            job: None,
        }
    }
}

impl MainWindow {
    pub(crate) fn note_download_query(&mut self, text: &str) {
        if self.download.downloading || self.download.importing {
            return;
        }
        if self.download.success.is_some() {
            self.download.success = None;
            self.download.success_at = None;
        }
        if !ytdlp::is_media_url(text) {
            self.clear_download_probe();
            return;
        }
        if self.download.ytdlp_ready.is_none() {
            self.download.ytdlp_ready = Some(ytdlp::ytdlp_available());
        }
        if self.download.ytdlp_ready == Some(false) {
            self.download.error = Some(MISSING_YTDLP.into());
            self.download.probing = false;
            self.download.probe_after = None;
            return;
        }
        let normalized = ytdlp::normalize_media_url(text).unwrap_or_default();
        if self.download.probed_query == normalized
            && (self.download.probe.is_some() || self.download.probing)
        {
            return;
        }
        self.download.pending_query = text.trim().to_string();
        self.download.probe_after = Some(Instant::now() + Duration::from_millis(320));
    }

    pub(crate) fn tick_download(&mut self, cx: &mut Context<Self>) -> bool {
        let mut dirty = false;
        if self
            .download
            .probe_after
            .is_some_and(|at| Instant::now() >= at)
            && !self.download.downloading
            && !self.download.importing
        {
            self.download.probe_after = None;
            self.start_probe(cx);
            dirty = true;
        }
        if let Some(job) = &self.download.job {
            let (percent, status) = job_progress(job);
            if (percent - self.download.percent).abs() > 0.4 || status != self.download.status {
                self.download.percent = percent;
                self.download.status = status;
                dirty = true;
            }
        }
        if self
            .download
            .success_at
            .is_some_and(|at| at.elapsed() >= Duration::from_millis(1800))
        {
            self.download.success = None;
            self.download.success_at = None;
            self.clear_download_probe();
            self.search
                .update(cx, |field, cx| field.replace_text("", cx));
            dirty = true;
        }
        if self.apply_search_lock(cx) {
            dirty = true;
        }
        dirty
    }

    pub(crate) fn url_panel(&mut self, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let disabled = self.download_disabled();
        let success = self.download.success.is_some();
        let label = self.download_label();
        let show_cancel =
            self.download.downloading && !self.download.importing && self.download.job.is_some();
        div()
            .id("search-url-panel")
            .occlude()
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, true, window.mouse_position(), cx);
                }),
            )
            .w_full()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(12.))
            .py(px(10.))
            .min_h(px(64.))
            .rounded_md()
            .bg(theme::bg_elevated())
            .border_1()
            .border_color(theme::border())
            .shadow_md()
            .child(self.url_body())
            .child(self.url_actions(label, disabled, success, show_cancel, cx))
    }

    fn url_body(&self) -> gpui::AnyElement {
        if let Some(probe) = &self.download.probe {
            let meta = probe_meta(probe);
            // One second line, same metrics as the search card. Import hides the status.
            let second = if let Some(error) = self.download.error.clone() {
                Some((error, true))
            } else if self.download.downloading && !self.download.importing {
                let status = self.download.status.clone();
                if status.is_empty() {
                    (!meta.is_empty()).then_some((meta, false))
                } else {
                    Some((status, false))
                }
            } else if !meta.is_empty() {
                Some((meta, false))
            } else {
                None
            };
            return div()
                .flex()
                .flex_1()
                .min_w(px(0.))
                .h(px(44.))
                .items_center()
                .gap(px(10.))
                .child(self.probe_thumb())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.))
                        .justify_center()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(16.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .truncate()
                                .child(probe.title.clone()),
                        )
                        .when_some(second, |col, (text, error)| {
                            col.child(
                                div()
                                    .text_size(px(11.))
                                    .line_height(px(14.))
                                    .text_color(if error {
                                        gpui::rgb(0xf87171)
                                    } else {
                                        theme::text_secondary()
                                    })
                                    .truncate()
                                    .child(text),
                            )
                        }),
                )
                .into_any_element();
        }
        if self.download.probing {
            return placeholder(false, "Recognizing link…", true);
        }
        if let Some(error) = &self.download.error {
            return placeholder(true, error, false);
        }
        placeholder(false, "Media link detected", false)
    }

    fn probe_thumb(&self) -> gpui::AnyElement {
        if let Some(path) = &self.download.thumb {
            return img(path.clone())
                .size(px(44.))
                .flex_shrink_0()
                .rounded(px(4.))
                .object_fit(ObjectFit::Cover)
                .into_any_element();
        }
        div()
            .size(px(44.))
            .flex_shrink_0()
            .rounded(px(4.))
            .bg(theme::bg_surface())
            .flex()
            .items_center()
            .justify_center()
            .child(icon(NOTE, theme::text_muted(), px(16.)))
            .into_any_element()
    }

    fn url_actions(
        &self,
        label: String,
        disabled: bool,
        success: bool,
        show_cancel: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.))
            .when(show_cancel, |row| row.child(self.cancel_button(cx)))
            .child(self.download_button(label, disabled, success, cx))
            .into_any_element()
    }

    fn cancel_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .id("download-cancel")
            .flex()
            .items_center()
            .justify_center()
            .h(px(34.))
            .px(px(14.))
            .rounded_sm()
            .bg(gpui::rgba(0xffffff0f))
            .whitespace_nowrap()
            .text_size(px(11.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::text_secondary())
            .cursor_pointer()
            .child("Cancel")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.arm("download-cancel", false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if !this.armed("download-cancel") {
                        return;
                    }
                    this.cancel_download(cx);
                }),
            )
            .into_any_element()
    }

    fn download_button(
        &self,
        label: String,
        disabled: bool,
        success: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .id("download-go")
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(px(34.))
            .px(px(14.))
            .rounded_sm()
            .whitespace_nowrap()
            .bg(if success {
                gpui::rgb(0x22c55e)
            } else {
                theme::accent()
            })
            .text_size(px(11.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(theme::white())
            .when(disabled, |button| button.opacity(0.5).cursor_default())
            .when(!disabled, |button| button.cursor_pointer())
            .when(!success, |button| {
                button.child(icon(ARROW, theme::white(), px(14.)))
            })
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if this.download_disabled() {
                        return;
                    }
                    this.arm("download-go", false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if !this.armed("download-go") || this.download_disabled() {
                        return;
                    }
                    this.begin_download(cx);
                }),
            )
            .into_any_element()
    }

    fn download_label(&self) -> String {
        if let Some(success) = &self.download.success {
            return success.clone();
        }
        if self.download.downloading || self.download.importing {
            return format!("{:.0}%", self.download.percent);
        }
        if self.download.probing {
            return "Checking…".into();
        }
        "Download".into()
    }

    fn download_disabled(&self) -> bool {
        self.download.probing
            || self.download.downloading
            || self.download.importing
            || self.download.ytdlp_ready == Some(false)
    }

    fn begin_download(&mut self, cx: &mut Context<Self>) {
        if self.download_disabled() {
            return;
        }
        let query = self.search.read(cx).content.to_string();
        let Some(url) = ytdlp::normalize_media_url(&query).filter(|url| ytdlp::is_media_url(url))
        else {
            return;
        };
        let playlist = self
            .download
            .probe
            .as_ref()
            .is_some_and(|probe| probe.is_playlist)
            || ytdlp::looks_like_playlist_url(&url);
        let named = if playlist {
            let title = self
                .download
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
                self.download.error = Some(error);
                cx.notify();
                return;
            }
        };
        let allow_playlist = named.is_some();
        self.download.download_gen = self.download.download_gen.wrapping_add(1);
        let gen = self.download.download_gen;
        self.download.downloading = true;
        self.download.importing = false;
        self.download.percent = 0.0;
        self.download.status = "Starting download…".into();
        self.download.error = None;
        self.download.success = None;
        self.download.success_at = None;
        let snap = Arc::new(Mutex::new(JobSnap::new()));
        self.download.job = Some(Arc::clone(&snap));
        self.apply_search_lock(cx);
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
                    if view.download.download_gen != gen {
                        return None;
                    }
                    view.download.job = None;
                    match result {
                        Err(error) => {
                            view.fail_download(error, cx);
                            None
                        }
                        Ok(files) => {
                            view.download.importing = true;
                            view.download.status = "Adding to library…".into();
                            view.apply_search_lock(cx);
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
                if view.download.download_gen != gen {
                    return;
                }
                view.finish_download_import(scanned, count, named, configured_id, dir, cx);
            });
        })
        .detach();
    }

    fn finish_download_import(
        &mut self,
        files: Vec<muzeeka::MusicFile>,
        count: usize,
        named: Option<String>,
        configured_id: Option<String>,
        dir: PathBuf,
        cx: &mut Context<Self>,
    ) {
        if files.is_empty() {
            self.fail_download("No supported audio files found".into(), cx);
            return;
        }
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
                if named.is_some() {
                    if let Some(id) = report.open_id.clone() {
                        if let Some(thumb) = self.download.thumb.clone() {
                            if thumb.is_file() {
                                self.session.update(cx, |session, _cx| {
                                    session.set_playlist_cover(&id, &thumb);
                                });
                            }
                        }
                    }
                }
                self.download.downloading = false;
                self.download.importing = false;
                self.download.percent = 100.0;
                self.download.status.clear();
                self.download.error = None;
                self.download.success = Some(if count == 1 {
                    "Added".into()
                } else {
                    format!("+{count}")
                });
                self.download.success_at = Some(Instant::now());
                self.download.pending_query = self.search.read(cx).content.to_string();
            }
            Err(error) => {
                self.fail_download(error, cx);
                return;
            }
        }
        self.apply_search_lock(cx);
        cx.notify();
    }

    fn fail_download(&mut self, error: String, cx: &mut Context<Self>) {
        self.download.downloading = false;
        self.download.importing = false;
        self.download.job = None;
        self.download.status.clear();
        if error != "Download cancelled" {
            self.download.error = Some(error);
        }
        self.apply_search_lock(cx);
        cx.notify();
    }

    fn cancel_download(&mut self, cx: &mut Context<Self>) {
        self.download.download_gen = self.download.download_gen.wrapping_add(1);
        self.download.downloading = false;
        self.download.importing = false;
        self.download.percent = 0.0;
        self.download.status.clear();
        if let Some(job) = self.download.job.take() {
            if let Ok(mut guard) = job.lock() {
                guard.cancel = true;
            } else if let Err(poison) = job.lock() {
                poison.into_inner().cancel = true;
            }
            std::thread::spawn(move || {
                for _ in 0..20 {
                    let pid = match job.lock() {
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
        self.apply_search_lock(cx);
        cx.notify();
    }

    fn clear_download_probe(&mut self) {
        self.download.probe_gen = self.download.probe_gen.wrapping_add(1);
        self.download.probing = false;
        self.download.probe = None;
        self.download.error = None;
        self.download.success = None;
        self.download.success_at = None;
        self.download.thumb = None;
        self.download.probe_after = None;
        self.download.probed_query.clear();
        self.download.pending_query.clear();
        self.download.status.clear();
    }

    fn apply_search_lock(&mut self, cx: &mut Context<Self>) -> bool {
        let locked = self.download.downloading || self.download.importing;
        self.search.update(cx, |field, cx| {
            if field.set_locked(locked) {
                cx.notify();
                true
            } else {
                false
            }
        })
    }

    fn start_probe(&mut self, cx: &mut Context<Self>) {
        let query = self.download.pending_query.clone();
        let Some(url) = ytdlp::normalize_media_url(&query).filter(|url| ytdlp::is_media_url(url))
        else {
            return;
        };
        self.download.probe_gen = self.download.probe_gen.wrapping_add(1);
        let gen = self.download.probe_gen;
        self.download.probed_query = url.clone();
        self.download.probing = true;
        self.download.probe = None;
        self.download.thumb = None;
        self.download.error = None;
        self.download.success = None;
        self.download.success_at = None;
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
                if view.download.probe_gen != gen {
                    return;
                }
                view.download.probing = false;
                match probed {
                    (Ok(probe), thumb) => {
                        view.download.probe = Some(probe);
                        view.download.thumb = thumb;
                        view.download.error = None;
                    }
                    (Err(error), _) => {
                        view.download.probe = None;
                        view.download.thumb = None;
                        view.download.error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn placeholder(error: bool, text: &str, spin: bool) -> gpui::AnyElement {
    div()
        .flex()
        .flex_1()
        .min_w(px(0.))
        .h(px(44.))
        .items_center()
        .gap(px(8.))
        .text_size(px(11.))
        .text_color(if error {
            gpui::rgb(0xf87171)
        } else {
            theme::text_secondary()
        })
        .when(spin, |row| {
            row.child(
                svg()
                    .data(SPINNER)
                    .text_color(theme::accent())
                    .size(px(14.))
                    .flex_shrink_0()
                    .with_animation(
                        "download-spin",
                        Animation::new(Duration::from_millis(700)).repeat(),
                        |glyph, phase| {
                            glyph.with_transformation(Transformation::rotate(gpui::radians(
                                phase * std::f32::consts::TAU,
                            )))
                        },
                    ),
            )
        })
        .child(div().min_w(px(0.)).truncate().child(text.to_string()))
        .into_any_element()
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
