//! Media-link cards under the search field.
//!
//! A pasted link leaves the field and becomes its own card. Cards stack with
//! the newest on top, and search results render above them. Several cards can
//! probe and download at once. yt-dlp runs in the background, then the files
//! go through the same library import as a file drop.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    div, img, prelude::*, px, svg, Animation, AnimationExt, Context, IntoElement, MouseButton,
    ObjectFit, Transformation,
};

use crate::ui::icons::icon;
use crate::ui::import::{self, ImportKind};
use crate::ui::main_window::MainWindow;
use crate::ui::session;
use crate::ui::theme;
use crate::ui::ytdlp::{self, JobSnap, Probe};

const MISSING_YTDLP: &str = "yt-dlp was not found. Place yt-dlp.exe in bin next to Muzeeka.";
/// Pause while a link is typed. A paste replaces the field and commits at once.
const TYPE_PAUSE: Duration = Duration::from_millis(320);

const NOTE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#efefef" d="M9 17.5a2.5 2.5 0 1 1-2-2.45V5.2l12-2v10.15a2.5 2.5 0 1 1-2-2.45V6.1l-8 1.35V17.5z"/></svg>"##;
const ARROW: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path fill="#efefef" d="M11 3h2v9.2l3.3-3.3 1.4 1.4L12 16.1 6.3 10.3l1.4-1.4L11 12.2V3zM5 19h14v2H5z"/></svg>"##;
const SPINNER: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="8" cy="2.2" r="1.7" fill="#efefef"/><circle cx="12.4" cy="4.6" r="1.45" fill="#efefef" opacity=".8"/><circle cx="13.8" cy="8.8" r="1.2" fill="#efefef" opacity=".55"/><circle cx="11.2" cy="12.4" r="1.05" fill="#efefef" opacity=".35"/><circle cx="7.2" cy="13.6" r="0.9" fill="#efefef" opacity=".2"/></svg>"##;

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

    fn label(&self) -> String {
        if let Some(success) = &self.success {
            return success.clone();
        }
        if self.downloading || self.importing {
            return format!("{:.0}%", self.percent);
        }
        if self.probing {
            return "Checking…".into();
        }
        "Download".into()
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
    /// Newest card first, so a new link sits under the field and older cards drop.
    jobs: Vec<DownloadJob>,
    next_id: u64,
    ytdlp_ready: Option<bool>,
    /// Link text from the previous search change. A paste is not an append.
    last_query: String,
    pending_urls: Vec<String>,
    pending_at: Option<Instant>,
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
        }
    }
}

struct CardSnap {
    id: u64,
    title: Option<String>,
    second: Option<(String, bool)>,
    thumb: Option<PathBuf>,
    probing: bool,
    label: String,
    disabled: bool,
    success: bool,
    show_cancel: bool,
    show_dismiss: bool,
}

impl MainWindow {
    pub(crate) fn has_download_cards(&self) -> bool {
        !self.download.jobs.is_empty()
    }

    /// The field was cleared without `on_change`. Drop the typed-link pause
    /// so the next paste is not treated as a continuation.
    pub(crate) fn note_search_cleared(&mut self) {
        self.download.last_query.clear();
        self.download.pending_urls.clear();
        self.download.pending_at = None;
    }

    /// A finished link leaves the field and opens a card. Typing keeps the
    /// text until the pause, so a half-written URL is not taken.
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
            self.download
                .jobs
                .retain(|job| !finished.contains(&job.id));
            dirty = true;
        }
        dirty
    }

    pub(crate) fn download_cards(&mut self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let missing = self.download.ytdlp_ready == Some(false);
        let snaps: Vec<CardSnap> = self
            .download
            .jobs
            .iter()
            .map(|job| card_snap(job, missing))
            .collect();
        snaps
            .into_iter()
            .map(|snap| self.download_card(snap, cx))
            .collect()
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
        // Reverse so the first link of this paste stays nearest the field.
        for url in urls.into_iter().rev() {
            self.enqueue_download(url, cx);
        }
        self.download.last_query.clear();
        self.search
            .update(cx, |field, cx| field.replace_text("", cx));
    }

    fn enqueue_download(&mut self, url: String, cx: &mut Context<Self>) {
        if self
            .download
            .jobs
            .iter()
            .any(|job| job.url == url && job.success.is_none())
        {
            return;
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
            return;
        }
        self.download.jobs.insert(0, job);
        self.start_probe(id, cx);
    }

    fn job_mut(&mut self, id: u64) -> Option<&mut DownloadJob> {
        self.download.jobs.iter_mut().find(|job| job.id == id)
    }

    fn download_card(&mut self, snap: CardSnap, cx: &mut Context<Self>) -> gpui::AnyElement {
        let id = snap.id;
        div()
            .id(("download-card", id))
            .occlude()
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.receive_file_drop(paths, true, window.mouse_position(), cx);
                }),
            )
            .w_full()
            .flex_shrink_0()
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
            .child(card_body(&snap))
            .child(self.card_actions(&snap, cx))
            .into_any_element()
    }

    fn card_actions(&mut self, snap: &CardSnap, cx: &mut Context<Self>) -> gpui::AnyElement {
        let id = snap.id;
        div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(6.))
            .when(snap.show_cancel, |row| row.child(self.cancel_button(id, cx)))
            .child(self.download_button(snap, cx))
            .when(snap.show_dismiss, |row| row.child(self.dismiss_button(id, cx)))
            .into_any_element()
    }

    fn cancel_button(&mut self, id: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let arm_id = format!("download-cancel-{id}");
        let release_id = arm_id.clone();
        div()
            .id(("download-cancel", id))
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

    fn dismiss_button(&mut self, id: u64, cx: &mut Context<Self>) -> gpui::AnyElement {
        let arm_id = format!("download-x-{id}");
        let release_id = arm_id.clone();
        div()
            .id(("download-x", id))
            .flex()
            .items_center()
            .justify_center()
            .size(px(34.))
            .rounded_sm()
            .text_size(px(16.))
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

    fn download_button(&mut self, snap: &CardSnap, cx: &mut Context<Self>) -> gpui::AnyElement {
        let id = snap.id;
        let disabled = snap.disabled;
        let success = snap.success;
        let label = snap.label.clone();
        let arm_id = format!("download-go-{id}");
        let release_id = arm_id.clone();
        div()
            .id(("download-go", id))
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
}

fn card_snap(job: &DownloadJob, ytdlp_missing: bool) -> CardSnap {
    let meta = job.probe.as_ref().map(probe_meta).unwrap_or_default();
    let second = if let Some(error) = job.error.clone() {
        Some((error, true))
    } else if job.downloading || job.importing {
        let status = job.status.clone();
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
    CardSnap {
        id: job.id,
        title: job.probe.as_ref().map(|probe| probe.title.clone()),
        second,
        thumb: job.thumb.clone(),
        probing: job.probing,
        label: job.label(),
        disabled: job.disabled(ytdlp_missing),
        success: job.success.is_some(),
        show_cancel: job.downloading && !job.importing && job.job.is_some(),
        show_dismiss: !job.importing,
    }
}

fn card_body(snap: &CardSnap) -> gpui::AnyElement {
    if let Some(title) = &snap.title {
        return div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .h(px(44.))
            .items_center()
            .gap(px(10.))
            .child(probe_thumb(snap.thumb.clone()))
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
                            .child(title.clone()),
                    )
                    .when_some(snap.second.clone(), |col, (text, error)| {
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
    if snap.probing {
        return placeholder(snap.id, false, "Recognizing link…", true);
    }
    if let Some((error, _)) = &snap.second {
        return placeholder(snap.id, true, error, false);
    }
    placeholder(snap.id, false, "Media link detected", false)
}

fn probe_thumb(path: Option<PathBuf>) -> gpui::AnyElement {
    if let Some(path) = path {
        return img(path)
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

fn placeholder(id: u64, error: bool, text: &str, spin: bool) -> gpui::AnyElement {
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
                        ("download-spin", id),
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

/// Every whitespace-separated token must be a specific media link.
/// A bare host stays in the field so a half-typed URL is not taken.
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
}
