use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    canvas, div, img, linear_color_stop, linear_gradient, point, prelude::*, px, relative,
    AnimationExt, AnyElement, BoxShadow, Context, Hsla, Image, MouseButton, ObjectFit,
    SharedString, SpringAnimation, SpringConfig, Window, WindowControlArea,
};

use crate::ui::cover::cover_art;
use crate::ui::icons::{self, icon};
use crate::ui::lyric_motion::{self, LineMotion, Lit};
use crate::ui::lyrics::{self, LyricLine, Lyrics, SyncType};
use crate::ui::main_window::MainWindow;
use crate::ui::session::{self, TrackId};
use crate::ui::theme;
use crate::ui::track_list::like_face;

const APPEAR_SEC: f32 = 0.48;
const CLOSE_SEC: f32 = 0.32;
const CHROME_IDLE: Duration = Duration::from_millis(1800);
const PANEL_LINGER: Duration = Duration::from_millis(900);
const PANEL_SLIDE_PX: f32 = 196.;
const BACKDROP_FADE_SEC: f32 = 0.7;
const TOP_BAR_H: f32 = 48.;
const BOTTOM_CHROME_H: f32 = 128.;

const WHEEL_MOVE_SEC: f32 = 0.28;
const USER_HOLD: Duration = Duration::from_secs(3);
const COLUMN_TOP_PAD: f32 = 80.;
const CLOCK_LATE_S: f64 = 0.06;

/// Unsung words. The white wipe is its own layer, so the active line stays this gray underneath.
const INACTIVE_ALPHA: f32 = 0.3;
/// Braccato `--blyrics-line-height`.
const LINE_HEIGHT: f32 = 1.333;
/// Slices of the Braccato mask gradient. The edge is 0.1 of the word, same as
/// `--mask-fade`, and these stops keep that ramp from turning into a hard cut.
const WIPE_BANDS: usize = 32;
const BACKGROUND_SCALE: f32 = 0.65;
/// Backing under a lead line. A line that is only backing keeps the line alpha.
const BACKGROUND_OPACITY: f32 = 0.7;
/// Peak opacity of the white copy drawn just above a letter during its wave.
const GLOW_ALPHA: f32 = 0.3;
const GLOW_LIFT_PX: f32 = 1.5;
/// A letter eases up and apart before its wave, then the wave takes over.
const PRE_LIFT_EM: f32 = 0.02;
const PRE_SPREAD_EM: f32 = 0.04;
/// `--blyrics-padding: 2rem`. Two lines leave twice this much air between them.
const LINE_PAD: f32 = 32.;
const WORD_SPACE_EM: f32 = 0.26;
/// Below this the swell is invisible, so the letter stays one text node.
const SCALE_SNAP: f32 = 1.004;
/// The letter mask may open this early. Layout and the sideways wobble wait
/// until the line itself starts, so the row does not change shape beforehand.
const PRE_ROLL_MS: f64 = lyric_motion::LETTER_PRE_ROLL_MS as f64;

pub(crate) struct Fullscreen {
    open: bool,
    opened_at: Instant,
    closing_at: Option<Instant>,
    close_from: f32,
    show_lyrics: bool,
    panel_toggled: Instant,
    last_move: Instant,
    chrome_hover: bool,
    lyrics: HashMap<TrackId, LyricsEntry>,
    loading: HashSet<TrackId>,
    backdrop: Backdrop,
    scroll: LyricScroll,
    measure: Rc<RefCell<Measure>>,
    clock: Clock,
    drift: f64,
    drift_at: Instant,
    full_covers: HashMap<PathBuf, PathBuf>,
    groups: GroupCache,
    scale_track: Option<TrackId>,
    scales: Vec<ScaleAnim>,
}

#[derive(Clone)]
struct ScaleAnim {
    from: f32,
    to: f32,
    started: Instant,
}

#[derive(Default)]
struct GroupCache {
    key: Option<(TrackId, usize)>,
    lines: Vec<(Vec<Group>, Vec<Group>)>,
    /// Quantized font and column width the row breaks were measured at.
    wrap_key: Option<(i32, i32)>,
    wraps: Vec<LineWrap>,
    /// Positions cache: [line_index][group_index] -> Vec<f32> (x positions for each letter)
    positions_key: Option<(i32, i32)>,
    positions: Vec<Vec<Vec<f32>>>,
}

struct LineWrap {
    main: Vec<Vec<usize>>,
    back: Vec<Vec<usize>>,
}

fn scale_value(anim: &ScaleAnim, now: Instant) -> f32 {
    let t = now.saturating_duration_since(anim.started).as_secs_f64()
        / (lyric_motion::LINE_SCALE_MS / 1000.0);
    let t = t.clamp(0.0, 1.0) as f32;
    anim.from + (anim.to - anim.from) * lyric_motion::EASE.at(t)
}

impl Fullscreen {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            open: false,
            opened_at: now,
            closing_at: None,
            close_from: 1.0,
            show_lyrics: true,
            panel_toggled: now,
            last_move: now,
            chrome_hover: false,
            lyrics: HashMap::new(),
            loading: HashSet::new(),
            backdrop: Backdrop::default(),
            scroll: LyricScroll::default(),
            measure: Rc::new(RefCell::new(Measure::default())),
            clock: Clock::default(),
            drift: 0.0,
            drift_at: now,
            full_covers: HashMap::new(),
            groups: GroupCache::default(),
            scale_track: None,
            scales: Vec::new(),
        }
    }

    /// Paint scale follows the target over [`lyric_motion::LINE_SCALE_MS`].
    /// A seek changes the target at once, and the line still eases instead of jumping.
    fn step_scale(&mut self, index: usize, target: f32, now: Instant) -> f32 {
        if index >= self.scales.len() {
            self.scales.resize(
                index + 1,
                ScaleAnim {
                    from: target,
                    to: target,
                    started: now - Duration::from_secs(1),
                },
            );
        }
        if (self.scales[index].to - target).abs() > 0.0008 {
            let shown = scale_value(&self.scales[index], now);
            self.scales[index] = ScaleAnim {
                from: shown,
                to: target,
                started: now,
            };
        }
        scale_value(&self.scales[index], now)
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    fn appear(&self, now: Instant) -> f32 {
        match self.closing_at {
            Some(at) => {
                let t = (now.duration_since(at).as_secs_f32() / CLOSE_SEC).min(1.0);
                self.close_from * (1.0 - ease_out_cubic(t))
            }
            None => {
                let t = (now.duration_since(self.opened_at).as_secs_f32() / APPEAR_SEC).min(1.0);
                ease_out_cubic(t)
            }
        }
    }

    fn line_offsets(
        &mut self,
        track: TrackId,
        lines: &[LyricLine],
        scroll_time: f64,
        untimed: bool,
    ) -> Option<Vec<f32>> {
        let now = Instant::now();
        let count = lines.len();
        let measure = self.measure.clone();
        let mut m = measure.borrow_mut();
        let s = &mut self.scroll;
        if s.track != Some(track) || s.count != count {
            *s = LyricScroll {
                track: Some(track),
                count,
                ..LyricScroll::default()
            };
            m.tops = vec![0.; count];
            m.heights = vec![0.; count];
        }
        if !m.valid(count) {
            m.applied = vec![0.; count];
            return None;
        }
        if s.shifts.len() != count {
            s.shifts = (0..count).map(|_| Vec::new()).collect();
        }
        for row in &mut s.shifts {
            row.retain(|shift| shift.alive(now));
        }
        if untimed {
            if !s.placed {
                s.glide = Glide::rest(0.0);
                s.placed = true;
            }
            let values = s.visuals(now);
            m.applied = values.clone();
            return Some(values);
        }

        let resume = s.user.is_some_and(|at| now.duration_since(at) > USER_HOLD);
        let held = s.user.is_some() && !resume;
        if held && s.placed {
            s.last_time = Some(scroll_time);
            let values = s.visuals(now);
            m.applied = values.clone();
            return Some(values);
        }
        if resume {
            s.user = None;
        }

        let boxes: Vec<lyric_motion::LyricBox> = (0..count)
            .map(|index| lyric_motion::LyricBox {
                time: lines[index].start_ms as f64 / 1000.0,
                duration: lines[index].duration_ms as f64 / 1000.0,
                top: m.tops[index],
                height: m.heights[index],
            })
            .collect();
        let jumped = s
            .last_time
            .is_some_and(|previous| (scroll_time - previous).abs() > 0.5);
        let force = s.force || jumped || resume;
        let Some(plan) =
            lyric_motion::plan_scroll(&boxes, scroll_time, m.view_h, &s.group, s.pinned, force)
        else {
            return None;
        };
        let current = s.glide.pos(now);
        if !s.placed {
            s.glide = Glide::rest(plan.target);
            s.group = plan.active;
            s.pinned = plan.pin;
            s.placed = true;
            s.force = false;
        } else if plan.commit {
            let delta = plan.target - current;
            // The scroll position jumps, and each visible line starts translated by
            // the same distance so the picture does not move. The translation then
            // eases to zero on its own duration. A later scroll adds another
            // translation instead of restarting the one already running.
            if delta.abs() > 2.0 {
                let distance = delta.abs();
                let step = lyric_motion::neighbor_step(&boxes, plan.focus);
                let view_h = m.view_h;
                for index in 0..count {
                    let line = &boxes[index];
                    if !lyric_motion::line_crosses_scroll(
                        line.top,
                        line.height,
                        current,
                        plan.target,
                        view_h,
                    ) {
                        continue;
                    }
                    let relative = index as i32 - plan.focus as i32;
                    s.shifts[index].push(Shift {
                        from: delta,
                        start: now,
                        dur: lyric_motion::line_shift_ms(
                            relative,
                            delta,
                            distance,
                            step,
                            plan.natural,
                        ) / 1000.0,
                    });
                }
            }
            s.glide = Glide::rest(plan.target);
            s.group = plan.active;
            s.pinned = plan.pin;
            s.force = false;
        } else if s.group == plan.active
            && s.shifts.iter().all(|row| row.is_empty())
            && s.glide.done(now)
            && (plan.target - current).abs() > 8.0
        {
            // The lookahead already moves the target. Following it here snapped
            // the line change instead of letting the commit slide. A drift with
            // the same group is the layout changing under a scroll that is done.
            s.glide = Glide::rest(plan.target);
        }
        s.last_time = Some(scroll_time);
        let values = s.visuals(now);
        m.applied = values.clone();
        Some(values)
    }

    fn wheel(&mut self, delta: f32, untimed: bool) {
        let m = self.measure.borrow();
        let count = m.tops.len();
        if !m.valid(count) || self.scroll.count != count || !self.scroll.placed {
            return;
        }
        let now = Instant::now();
        let (lo, hi) = scroll_range(&m, untimed);
        let s = &mut self.scroll;
        let current = s.glide.pos(now);
        s.glide = Glide {
            from: current,
            to: (current - delta).clamp(lo, hi),
            start: now,
            dur: WHEEL_MOVE_SEC,
        };
        s.user = Some(now);
    }
}

struct LyricsEntry {
    lyrics: Option<Arc<Lyrics>>,
    stamp: Option<(PathBuf, Option<SystemTime>)>,
    checked: Instant,
}

fn stamp_of(path: Option<&Path>) -> Option<(PathBuf, Option<SystemTime>)> {
    path.map(|path| {
        let modified = std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .ok();
        (path.to_path_buf(), modified)
    })
}

#[derive(Default)]
struct Clock {
    shown: f64,
    real: f64,
    at: Option<Instant>,
    track: Option<TrackId>,
    playing: bool,
}

impl Clock {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn time(&mut self, track: Option<TrackId>, real: f64, playing: bool) -> f64 {
        let now = Instant::now();
        let switched = self.at.is_none() || self.track != track || self.playing != playing;
        let dt = self
            .at
            .map(|at| now.saturating_duration_since(at).as_secs_f64().min(0.25))
            .unwrap_or(0.0);
        if !playing || switched {
            self.shown = real;
        } else if real != self.real {
            let predicted = self.shown + dt;
            let error = real - predicted;
            // A sample that arrives a little late must not pull the wipe backwards.
            // A seek, or a sample that is ahead, is taken whole.
            if error < -0.001 && error > -CLOCK_LATE_S {
                self.shown = predicted + error * 0.15;
            } else {
                self.shown = real;
            }
        } else {
            self.shown += dt;
        }
        self.real = real;
        self.track = track;
        self.playing = playing;
        self.at = Some(now);
        self.shown
    }
}

#[derive(Clone, Copy)]
struct Glide {
    from: f32,
    to: f32,
    start: Instant,
    dur: f32,
}

impl Default for Glide {
    fn default() -> Self {
        Self::rest(0.0)
    }
}

impl Glide {
    fn rest(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            dur: 0.0,
        }
    }

    fn pos(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        if self.dur <= 0.0 || t >= self.dur {
            return self.to;
        }
        self.from + (self.to - self.from) * ease_out_cubic(t / self.dur)
    }

    fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start).as_secs_f32() >= self.dur
    }
}

struct Shift {
    from: f32,
    start: Instant,
    dur: f32,
}

impl Shift {
    fn value(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.start).as_secs_f32();
        if self.dur <= 0.0 || t >= self.dur {
            return 0.0;
        }
        self.from * (1.0 - lyric_motion::SCROLL_EASE.at(t / self.dur))
    }

    fn alive(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start).as_secs_f32() < self.dur
    }
}

#[derive(Default)]
struct LyricScroll {
    track: Option<TrackId>,
    count: usize,
    glide: Glide,
    shifts: Vec<Vec<Shift>>,
    group: Vec<usize>,
    pinned: Option<usize>,
    last_time: Option<f64>,
    user: Option<Instant>,
    force: bool,
    placed: bool,
}

impl LyricScroll {
    fn visuals(&self, now: Instant) -> Vec<f32> {
        (0..self.count)
            .map(|index| {
                let extra: f32 = self.shifts[index]
                    .iter()
                    .map(|shift| shift.value(now))
                    .sum();
                self.glide.pos(now) - extra
            })
            .collect()
    }

    fn animating(&self, now: Instant) -> bool {
        !self.glide.done(now)
            || self
                .shifts
                .iter()
                .any(|row| row.iter().any(|shift| shift.alive(now)))
    }
}

#[derive(Default)]
struct Measure {
    view_top: f32,
    view_h: f32,
    tops: Vec<f32>,
    heights: Vec<f32>,
    applied: Vec<f32>,
}

impl Measure {
    fn valid(&self, count: usize) -> bool {
        count > 0
            && self.view_h > 0.0
            && self.tops.len() == count
            && self.heights.len() == count
            && self.heights.iter().all(|height| *height > 0.0)
    }

    fn content_h(&self) -> f32 {
        match (self.tops.last(), self.heights.last()) {
            (Some(top), Some(height)) => top + height,
            _ => 0.0,
        }
    }
}

fn scroll_range(m: &Measure, untimed: bool) -> (f32, f32) {
    let content = m.content_h();
    if untimed || m.tops.is_empty() {
        return (0.0, (content - m.view_h * 0.7).max(0.0));
    }
    let place = |index: usize| {
        m.tops[index] + m.heights[index] * 0.5 - m.view_h * lyric_motion::TARGET_RATIO
    };
    let lo = place(0);
    let hi = place(m.tops.len() - 1).max(lo);
    (lo, hi)
}

#[derive(Default)]
struct Backdrop {
    key: Option<PathBuf>,
    current: Option<(Arc<Image>, Instant)>,
    previous: Option<Arc<Image>>,
    cache: HashMap<PathBuf, Option<Arc<Image>>>,
    pending: HashSet<PathBuf>,
}

impl Backdrop {
    fn fade(&self, now: Instant) -> f32 {
        self.current
            .as_ref()
            .map(|(_, at)| {
                let t = (now.duration_since(*at).as_secs_f32() / BACKDROP_FADE_SEC).min(1.0);
                ease_out_cubic(t)
            })
            .unwrap_or(1.0)
    }
}

fn blur_cover(path: &Path) -> Option<Arc<Image>> {
    let source = image::open(path).ok()?;
    let small = source.resize_exact(72, 72, image::imageops::FilterType::Triangle);
    let blurred = small.blur(6.0).to_rgb8();
    let mut bytes = Vec::new();
    blurred
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .ok()?;
    Some(Arc::new(Image::from_bytes(gpui::ImageFormat::Png, bytes)))
}

fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn white_a(a: f32) -> Hsla {
    Hsla {
        h: 0.,
        s: 0.,
        l: 1.,
        a: a.clamp(0.0, 1.0),
    }
}

fn black_a(a: f32) -> Hsla {
    Hsla {
        h: 0.,
        s: 0.,
        l: 0.,
        a: a.clamp(0.0, 1.0),
    }
}

fn duration_secs(duration: f64) -> Option<u32> {
    (duration.is_finite() && duration > 0.0).then(|| duration.round() as u32)
}

fn duration_ms(duration: f64) -> u32 {
    if duration.is_finite() && duration > 0.0 {
        (duration * 1000.0).round() as u32
    } else {
        0
    }
}

struct Part {
    text: String,
    start_ms: f64,
    duration_ms: f32,
}

struct Group {
    parts: Vec<Part>,
    space_after: bool,
}

#[derive(Clone, Copy)]
enum Align {
    Start,
    Center,
    End,
}

#[derive(Clone, Copy)]
struct LineView {
    motion: LineMotion,
    /// The line's own clock has started. The wipe may paint earlier; the wobble may not.
    live: bool,
    now_ms: f64,
    base: f32,
    px_scale: f32,
    detailed: bool,
}

struct Unit {
    text: String,
    lit: Lit,
    highlight: f32,
    lift: f32,
    /// The letter is still inside its rise. A settled letter snaps to one pixel.
    traveling: bool,
    /// Paint scale of one letter. The layout box stays at 1.
    scale: f32,
    rtl: bool,
    /// White copy under the glyph. Zero except while the letter's wave is up.
    glow_alpha: f32,
    /// Paint-only horizontal offset. Layout width stays the measured row.
    spread: f32,
    /// Absolute x position if word is shaped. None means flex positioning.
    abs_x: Option<f32>,
}

fn snap(value: f32, px_scale: f32, steps: f32) -> f32 {
    let unit = px_scale.max(1.0) * steps;
    (value * unit).round() / unit
}

fn is_cjk(ch: char) -> bool {
    ch as u32 >= 0x2E80
}

fn splittable(text: &str) -> bool {
    !text.chars().any(|ch| {
        matches!(ch as u32,
            0x0590..=0x08FF | 0x0900..=0x0DFF | 0x0E00..=0x0FFF | 0x1000..=0x109F
            | 0x1780..=0x18AF | 0xA8E0..=0xA8FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
    })
}

fn is_rtl(text: &str) -> bool {
    text.chars()
        .any(|ch| matches!(ch as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF))
}

fn is_mark(ch: char) -> bool {
    matches!(ch as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF
        | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0x1F3FB..=0x1F3FF | 0xE0100..=0xE01EF)
}

fn letters_of(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut joined = false;
    for ch in text.chars() {
        let attach = joined || is_mark(ch) || ch == '\u{200D}';
        joined = ch == '\u{200D}';
        match out.last_mut() {
            Some(last) if attach => last.push(ch),
            _ => out.push(ch.to_string()),
        }
    }
    out
}

fn groups_of(parts: Vec<Part>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut current: Option<Group> = None;
    for part in parts {
        if part.text.starts_with(char::is_whitespace) {
            if let Some(mut group) = current.take() {
                group.space_after = true;
                groups.push(group);
            }
        }
        let ends_group = part.text.ends_with(char::is_whitespace);
        let core = part.text.trim().to_string();
        let split = core.chars().last().is_some_and(is_cjk);
        if !core.is_empty() {
            current
                .get_or_insert_with(|| Group {
                    parts: Vec::new(),
                    space_after: false,
                })
                .parts
                .push(Part { text: core, ..part });
        }
        if ends_group || split {
            if let Some(mut group) = current.take() {
                group.space_after = ends_group;
                groups.push(group);
            }
        }
    }
    groups.extend(current);
    groups
}

fn line_parts(text: &str, start_ms: f64) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut token = String::new();
    for ch in text.chars() {
        token.push(ch);
        if ch.is_whitespace() || is_cjk(ch) {
            parts.push(Part {
                text: std::mem::take(&mut token),
                start_ms,
                duration_ms: 0.0,
            });
        }
    }
    if !token.is_empty() {
        parts.push(Part {
            text: token,
            start_ms,
            duration_ms: 0.0,
        });
    }
    parts
}

fn backing_is_the_line(line: &LyricLine, backs: &[Part]) -> bool {
    if backs.is_empty() || line.parts.iter().any(|part| !part.background) {
        return false;
    }
    let backing: String = backs.iter().map(|part| part.text.as_str()).collect();
    let backing = backing.split_whitespace().collect::<Vec<_>>().join(" ");
    let words = line.words.split_whitespace().collect::<Vec<_>>().join(" ");
    words.is_empty() || words == backing
}

fn line_groups(line: &LyricLine, sync: SyncType) -> (Vec<Group>, Vec<Group>) {
    let mut main = Vec::new();
    let mut back = Vec::new();
    for part in &line.parts {
        let entry = Part {
            text: part.words.clone(),
            start_ms: part.start_ms as f64,
            duration_ms: part.duration_ms as f32,
        };
        if part.background {
            back.push(entry);
        } else {
            main.push(entry);
        }
    }
    let main = if sync == SyncType::Rich && !main.is_empty() {
        groups_of(main)
    } else if backing_is_the_line(line, &back) {
        Vec::new()
    } else {
        groups_of(line_parts(&line.words, line.start_ms as f64))
    };
    (main, groups_of(back))
}

fn part_units_with_shaping(
    window: &mut Window,
    part: &Part,
    view: LineView,
    font: f32,
    placed: Option<&[f32]>,
) -> (Vec<Unit>, f32) {
    let motion = view.motion;
    let rtl = is_rtl(&part.text);
    let since = (view.now_ms - part.start_ms) as f32;
    let fade = if motion.still { 0.0 } else { motion.highlight };
    let long = part.duration_ms > lyric_motion::LONG_WORD_MS;

    if part.duration_ms <= 0.0 || !splittable(&part.text) {
        let whole = |lit: Lit, highlight: f32| {
            (
                vec![Unit {
                    text: part.text.clone(),
                    lit,
                    highlight,
                    lift: 0.0,
                    traveling: false,
                    scale: 1.0,
                    rtl,
                    glow_alpha: 0.0,
                    spread: 0.0,
                    abs_x: None,
                }],
                0.0,
            )
        };
        if part.duration_ms <= 0.0 {
            return whole(Lit::FULL, lyric_motion::line_synced_fade(since) * fade);
        }
        let start = lyric_motion::swipe_start(since, part.duration_ms);
        return whole(lyric_motion::word_lit(start), if since >= 0.0 { fade } else { 0.0 });
    }

    // Positions from the whole word keep syllables on the same advances as the
    // idle line. Shaping each syllable alone, then gapping the boxes, prints
    // "los er" for "loser".
    let (positions, word_width) = match placed {
        Some(xs) => (xs.to_vec(), 0.0),
        None => shape_word(window, &part.text, font, gpui::FontWeight::BOLD),
    };

    let start = lyric_motion::swipe_start(since, part.duration_ms);
    let letters = letters_of(&part.text);
    let count = letters.len();
    let waves = lyric_motion::letter_waves(count, part.duration_ms);

    let units = letters
        .into_iter()
        .zip(waves)
        .enumerate()
        .map(|(index, (text, wave))| {
            let local = since - wave.delay_ms;
            let pre = lyric_motion::pre_roll_lit(since, wave.delay_ms);
            let real = lyric_motion::letter_lit(start, index, count);
            let approach = if pre <= 0.0 {
                0.0
            } else if local < 0.0 {
                (pre / lyric_motion::LETTER_PRE_ROLL_PEAK).clamp(0.0, 1.0)
            } else {
                (1.0 - (local / 200.0).clamp(0.0, 1.0)).max(0.0)
            };

            let lift_amount = if long {
                lyric_motion::float(since, wave).max(PRE_LIFT_EM * approach) * font * fade
            } else {
                0.0
            };

            Unit {
                text,
                lit: Lit {
                    edge: lyric_motion::pre_roll_edge(real, pre),
                    fade: real.fade,
                },
                highlight: if since >= 0.0 || pre > 0.0 { fade } else { 0.0 },
                lift: lift_amount,
                traveling: (local >= 0.0 && local < wave.duration_ms) || (local < 0.0 && pre > 0.0),
                scale: if long {
                    lyric_motion::emphasis(since, wave)
                } else {
                    1.0
                },
                rtl,
                glow_alpha: lyric_motion::wave_glow(since, wave) * GLOW_ALPHA * fade,
                spread: 0.0,
                abs_x: positions.get(index).copied(),
            }
        })
        .collect();

    (units, word_width)
}

fn clip_layer(text: &SharedString, from: f32, to: f32, color: Hsla) -> Option<AnyElement> {
    let width = to - from;
    if width <= 0.0005 {
        return None;
    }
    Some(
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(relative(from))
            .w(relative(width))
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(relative(-from / width))
                    .whitespace_nowrap()
                    .text_color(color)
                    .child(text.clone()),
            )
            .into_any_element(),
    )
}

fn unit_text(unit: &Unit, base: f32) -> gpui::Div {
    let text = SharedString::from(unit.text.clone());
    // One glyph already mixes the white reveal with the gray base.
    let flat = |reveal: f32| reveal + base * (1.0 - reveal);
    if unit.highlight <= 0.0 || unit.lit.dark() {
        return div().text_color(white_a(base)).child(text);
    }
    if unit.lit.full() {
        return div().text_color(white_a(flat(unit.highlight))).child(text);
    }
    let lo = unit.lit.edge.clamp(0.0, 1.0);
    let hi = (unit.lit.edge + unit.lit.fade).clamp(0.0, 1.0);
    let mut layers: Vec<AnyElement> = Vec::new();
    let mut push = |from: f32, to: f32, reveal: f32| {
        if reveal <= 0.004 {
            return;
        }
        let (from, to) = if unit.rtl {
            (1.0 - to, 1.0 - from)
        } else {
            (from, to)
        };
        layers.extend(clip_layer(&text, from, to, white_a(reveal)));
    };
    // The clip sits on top of the gray glyph. Its alpha is only the white
    // reveal. Mixing the base in here as well paints that gray twice.
    push(0.0, lo, unit.highlight);
    for band in 0..WIPE_BANDS {
        let from = lo + (hi - lo) * band as f32 / WIPE_BANDS as f32;
        let to = lo + (hi - lo) * (band + 1) as f32 / WIPE_BANDS as f32;
        push(from, to, unit.lit.at((from + to) * 0.5) * unit.highlight);
    }
    div()
        .relative()
        .child(div().text_color(white_a(base)).child(text))
        .children(layers)
}

/// GPUI has no glyph shadow. A second white copy, shifted up, is the halo.
fn with_glow(unit: &Unit, text: gpui::Div, glyph_font: f32) -> gpui::Div {
    if unit.glow_alpha <= 0.001 {
        return text;
    }
    div()
        .relative()
        .whitespace_nowrap()
        .child(
            div()
                .absolute()
                .top(px(-GLOW_LIFT_PX))
                .left_0()
                .whitespace_nowrap()
                .text_size(px(glyph_font))
                .line_height(px(glyph_font * LINE_HEIGHT))
                .text_color(white_a(unit.glow_alpha))
                .child(unit.text.clone()),
        )
        .child(text)
}

fn unit_body(unit: &Unit, font: f32, base: f32) -> gpui::Div {
    let glyph = |glyph_font: f32| {
        let mut text = unit_text(unit, base).whitespace_nowrap();
        if (glyph_font - font).abs() > 0.01 {
            text = text
                .text_size(px(glyph_font))
                .line_height(px(glyph_font * LINE_HEIGHT));
        }
        with_glow(unit, text, glyph_font)
    };
    if unit.scale <= SCALE_SNAP {
        return glyph(font);
    }
    // GPUI has no paint-only scale. The invisible copy keeps the letter's box,
    // and the visible copy grows from the center without pushing its neighbors.
    let size = font * unit.scale;
    div()
        .relative()
        .whitespace_nowrap()
        .child(unit_text(unit, base).whitespace_nowrap().opacity(0.))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_center()
                .child(glyph(size)),
        )
}

fn unit_element(unit: Unit, font: f32, base: f32, px_scale: f32) -> AnyElement {
    // Absolutely positioned letter (when abs_x is set)
    if let Some(x) = unit.abs_x {
        let body = unit_body(&unit, font, base);
        return div()
            .absolute()
            .left(px(x))
            .top(px(unit.lift))
            .w_auto()  // Critical: don't stretch to full width
            .h_auto()
            .child(body)
            .into_any_element();
    }

    // Flex positioned letter (default, for line-synced or unsplittable)
    let quantum = 1.0 / px_scale.max(1.0);
    let steps = unit.lift / quantum;
    let primary_step = steps.round();
    let primary_y = primary_step * quantum;
    let residual = if unit.traveling {
        steps - primary_step
    } else {
        0.0
    };
    if primary_y.abs() <= 0.04 && residual.abs() <= 0.04 {
        return div()
            .relative()
            .child(unit_body(&unit, font, base))
            .into_any_element();
    }
    let shell = div().relative().whitespace_nowrap().child(
        div()
            .opacity(0.)
            .whitespace_nowrap()
            .child(unit.text.clone()),
    );
    let placed = |y: f32, opacity: f32| {
        div()
            .absolute()
            .top(px(-y))
            .left_0()
            .opacity(opacity)
            .child(unit_body(&unit, font, base))
    };
    if residual.abs() <= 0.04 {
        return shell.child(placed(primary_y, 1.0)).into_any_element();
    }
    let ghost_y = (primary_step + residual.signum()) * quantum;
    shell
        .child(placed(primary_y, 1.0))
        .child(placed(ghost_y, residual.abs() * 0.65))
        .into_any_element()
}

fn group_shift(
    still: bool,
    rich: bool,
    since_ms: f32,
    font: f32,
    highlight: f32,
    px_scale: f32,
) -> f32 {
    if still || !rich {
        return 0.0;
    }
    let em = lyric_motion::wobble(since_ms);
    snap(em * font * highlight, px_scale, 4.0)
}

fn group_element(
    group: &Group,
    view: LineView,
    font: f32,
    pull_next: bool,
    align: Align,
    rich: bool,
    window: &mut Window,
) -> AnyElement {
    let tighten = |el: gpui::Div| el.when(pull_next, |el| el.mr(px(font * -WORD_SPACE_EM)));
    if !view.detailed {
        let text: String = group.parts.iter().map(|part| part.text.as_str()).collect();
        return tighten(div().text_color(white_a(view.base)).whitespace_nowrap())
            .child(text)
            .into_any_element();
    }
    let first_ms = group.parts.first().map_or(0.0, |part| part.start_ms);
    let shift = group_shift(
        !view.live,
        rich,
        (view.now_ms - first_ms) as f32,
        font,
        view.motion.highlight,
        view.px_scale,
    );
    let counts: Vec<usize> = group
        .parts
        .iter()
        .map(|part| letters_of(&part.text).len())
        .collect();
    let joined: String = group.parts.iter().map(|part| part.text.as_str()).collect();
    let all_split = group.parts.iter().all(|part| {
        part.duration_ms > 0.0 && splittable(&part.text) && !part.text.is_empty()
    });
    let shared = all_split
        && counts.iter().sum::<usize>() == letters_of(&joined).len()
        && !joined.is_empty();
    let (places, group_width) = if shared {
        shape_word(window, &joined, font, gpui::FontWeight::BOLD)
    } else {
        (Vec::new(), 0.0)
    };
    let shared = shared && places.len() == counts.iter().sum::<usize>();

    let mut cursor = 0usize;
    let mut letters = Vec::new();
    for (part, count) in group.parts.iter().zip(counts) {
        let xs = if shared {
            let end = (cursor + count).min(places.len());
            places.get(cursor..end)
        } else {
            None
        };
        cursor += count;
        let (units, _) = part_units_with_shaping(window, part, view, font, xs);
        letters.extend(units);
    }

    let row = if shared {
        div()
            .relative()
            .left(px(shift))
            .w(px(group_width))
            .h(px(font * LINE_HEIGHT))
            .children(
                letters
                    .into_iter()
                    .map(|unit| unit_element(unit, font, view.base, view.px_scale)),
            )
    } else {
        div()
            .relative()
            .left(px(shift))
            .flex()
            .flex_row()
            .children(
                letters
                    .into_iter()
                    .map(|unit| unit_element(unit, font, view.base, view.px_scale)),
            )
    };

    let row = match align {
        Align::Start => row.justify_start(),
        Align::Center => row.justify_center(),
        Align::End => row.justify_end(),
    };

    tighten(row).into_any_element()
}

fn word_row(
    groups: &[Group],
    indices: &[usize],
    view: LineView,
    font: f32,
    align: Align,
    rich: bool,
    window: &mut Window,
) -> gpui::Div {
    let last = indices.len().saturating_sub(1);
    let row = div()
        .w_full()
        .flex()
        .flex_row()
        .gap_x(px(font * WORD_SPACE_EM))
        .children(indices.iter().enumerate().filter_map(|(place, index)| {
            let group = groups.get(*index)?;
            let pull_next = !group.space_after && place < last;
            Some(group_element(group, view, font, pull_next, align, rich, window))
        }));
    match align {
        Align::Start => row.justify_start(),
        Align::Center => row.justify_center(),
        Align::End => row.justify_end(),
    }
}

/// Breaks chosen at the full font size. Later paint, including the 0.95 scale,
/// keeps these rows, so a growing line does not wrap again and jump.
fn pack_rows(widths: &[f32], gap_before: &[f32], max_w: f32) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut row: Vec<usize> = Vec::new();
    let mut used = 0.0;
    for (index, &width) in widths.iter().enumerate() {
        let gap = if row.is_empty() {
            0.0
        } else {
            gap_before.get(index).copied().unwrap_or(0.0)
        };
        if !row.is_empty() && used + gap + width > max_w {
            rows.push(std::mem::take(&mut row));
            used = 0.0;
        }
        let gap = if row.is_empty() { 0.0 } else { gap };
        used += gap + width;
        row.push(index);
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows
}

fn resolved_rows(count: usize, planned: &[Vec<usize>]) -> Vec<Vec<usize>> {
    if count == 0 {
        return Vec::new();
    }
    if planned.is_empty() {
        return vec![(0..count).collect()];
    }
    planned.to_vec()
}

fn text_width(window: &mut Window, text: &str, font_px: f32, weight: gpui::FontWeight) -> f32 {
    if text.is_empty() || font_px <= 0.0 {
        return 0.0;
    }
    let mut font = gpui::font(theme::FONT_FAMILY);
    font.weight = weight;
    let run = gpui::TextRun {
        len: text.len(),
        font,
        ..Default::default()
    };
    window
        .text_system()
        .shape_line(SharedString::from(text), px(font_px), &[run], None)
        .width()
        .as_f32()
}

fn shape_word(
    window: &mut Window,
    text: &str,
    font_px: f32,
    weight: gpui::FontWeight,
) -> (Vec<f32>, f32) {
    if text.is_empty() || font_px <= 0.0 {
        return (Vec::new(), 0.0);
    }
    let mut font = gpui::font(theme::FONT_FAMILY);
    font.weight = weight;
    let run = gpui::TextRun {
        len: text.len(),
        font,
        ..Default::default()
    };
    let line = window
        .text_system()
        .shape_line(SharedString::from(text), px(font_px), &[run], None);

    let letters = letters_of(text);
    let mut positions = Vec::new();
    let mut byte_pos = 0;

    for letter in letters {
        let x = line.x_for_index(byte_pos).as_f32();
        positions.push(x);
        byte_pos += letter.len();
    }

    let width = line.width().as_f32();
    (positions, width)
}

fn group_width(window: &mut Window, group: &Group, font_px: f32, weight: gpui::FontWeight) -> f32 {
    group
        .parts
        .iter()
        .map(|part| text_width(window, &part.text, font_px, weight))
        .sum()
}

fn measure_rows(
    window: &mut Window,
    groups: &[Group],
    font_px: f32,
    weight: gpui::FontWeight,
    max_w: f32,
) -> Vec<Vec<usize>> {
    if groups.is_empty() {
        return Vec::new();
    }
    let gap_px = font_px * WORD_SPACE_EM;
    let widths: Vec<f32> = groups
        .iter()
        .map(|group| group_width(window, group, font_px, weight))
        .collect();
    let gap_before: Vec<f32> = groups
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index > 0 && groups[index - 1].space_after {
                gap_px
            } else {
                0.0
            }
        })
        .collect();
    pack_rows(&widths, &gap_before, max_w)
}

/// The row keeps its full-size box. A smaller copy is painted from the line's
/// origin, the way `transform: scale(0.95)` does, so the scroll measure holds still.
fn resting_scale(full: AnyElement, small: AnyElement, align: Align, size: f32) -> AnyElement {
    div()
        .relative()
        .w_full()
        .child(div().w_full().opacity(0.).child(full))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .text_size(px(size))
                .line_height(px(size * LINE_HEIGHT))
                .when(matches!(align, Align::Start), |row| row.justify_start())
                .when(matches!(align, Align::End), |row| row.justify_end())
                .when(matches!(align, Align::Center), |row| row.justify_center())
                .child(small),
        )
        .into_any_element()
}

fn text_block(
    groups: &[Group],
    planned: &[Vec<usize>],
    view: LineView,
    font: f32,
    align: Align,
    rich: bool,
    window: &mut Window,
) -> gpui::Div {
    let rows = resolved_rows(groups.len(), planned);
    div().w_full().flex().flex_col().children(
        rows.iter()
            .map(|row| word_row(groups, row, view, font, align, rich, window)),
    )
}

fn lyric_line(
    main: &[Group],
    back: &[Group],
    main_rows: &[Vec<usize>],
    back_rows: &[Vec<usize>],
    view: LineView,
    font: f32,
    align: Align,
    rich: bool,
    window: &mut Window,
) -> AnyElement {
    let back_font = font * BACKGROUND_SCALE;
    div()
        .w_full()
        .flex()
        .flex_col()
        .when(matches!(align, Align::Start), |column| column.items_start())
        .when(matches!(align, Align::Center), |column| {
            column.items_center()
        })
        .when(matches!(align, Align::End), |column| column.items_end())
        .when(!main.is_empty(), |column| {
            column.child(text_block(main, main_rows, view, font, align, rich, window))
        })
        .when(!back.is_empty(), |column| {
            column.child(
                text_block(back, back_rows, view, back_font, align, rich, window)
                    .when(!main.is_empty(), |row| {
                        row.mt(px(font * 0.1)).opacity(BACKGROUND_OPACITY)
                    })
                    .text_size(px(back_font))
                    .line_height(px(back_font * LINE_HEIGHT))
                    .font_weight(gpui::FontWeight::MEDIUM),
            )
        })
        .into_any_element()
}

/// Opacity and paint scale for a line this many rows from the one being sung.
/// GPUI cannot blur text, so a far line is smaller and dimmer.
fn line_depth(distance: i32) -> (f32, f32) {
    match distance {
        0 => (1.0, 1.0),
        1 => (0.45, 0.99),
        _ => (0.3, 0.98),
    }
}

/// Ink of a line before the white wipe. A line-synced line keeps the neighbor's
/// brightness when it becomes current, so the fade up does not dip through the
/// darker unsung-word gray first.
fn line_ink(distance: i32, rich: bool) -> f32 {
    if distance == 0 && !rich {
        line_depth(1).0
    } else if distance == 0 {
        INACTIVE_ALPHA
    } else {
        line_depth(distance).0
    }
}

fn pause_factor(phase: f32) -> f32 {
    0.88 + 0.12 * phase.clamp(0.0, 1.0)
}

fn break_dots(progress: f32, phase: f32, font: f32, pulse: f32) -> AnyElement {
    let dot = font * 0.32;
    let dots = (0..3).map(|k| {
        let lit = (progress * 3.0 - k as f32).clamp(0.0, 1.0);
        let on = 0.25 + 0.75 * lit;
        let alpha = (0.25 + (on - 0.25) * phase) * (1.0 - (1.0 - pulse) * phase);
        div()
            .size(px(dot))
            .rounded_full()
            .bg(white_a(alpha))
            .into_any_element()
    });
    div()
        .h(px(font * 1.15))
        .flex()
        .items_center()
        .gap(px(dot * 0.7))
        .children(dots)
        .into_any_element()
}

impl MainWindow {
    pub(crate) fn open_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.read(cx).current.is_none() {
            return;
        }
        let fs = &mut self.fullscreen;
        if fs.open && fs.closing_at.is_none() {
            return;
        }
        let now = Instant::now();
        fs.open = true;
        fs.opened_at = now;
        fs.closing_at = None;
        fs.last_move = now;
        fs.drift_at = now;
        fs.chrome_hover = false;
        fs.clock.reset();
        fs.scroll = LyricScroll::default();
        self.armed_ui = None;
        self.hovered_cover = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(crate) fn close_fullscreen(&mut self, cx: &mut Context<Self>) {
        let fs = &mut self.fullscreen;
        if !fs.open || fs.closing_at.is_some() {
            return;
        }
        let now = Instant::now();
        fs.close_from = fs.appear(now);
        fs.closing_at = Some(now);
        fs.chrome_hover = false;
        cx.notify();
    }

    pub(crate) fn fullscreen_covered(&self) -> bool {
        let fs = &self.fullscreen;
        fs.open && fs.closing_at.is_none() && fs.opened_at.elapsed().as_secs_f32() >= APPEAR_SEC
    }

    fn ensure_lyrics(&mut self, id: TrackId, cx: &mut Context<Self>) {
        if self.fullscreen.loading.contains(&id) {
            return;
        }
        let Some(track) = self.session.read(cx).track(id).cloned() else {
            return;
        };
        let now = Instant::now();
        if let Some(entry) = self.fullscreen.lyrics.get_mut(&id) {
            if now.duration_since(entry.checked) < Duration::from_secs(1) {
                return;
            }
            entry.checked = now;
            if entry.stamp == stamp_of(track.lyrics.as_deref()) {
                return;
            }
        }
        self.fullscreen.loading.insert(id);
        cx.spawn(async move |this, cx| {
            let (parsed, fetched) = cx
                .background_executor()
                .spawn(async move {
                    let cached = track
                        .lyrics
                        .as_deref()
                        .and_then(|path| std::fs::read_to_string(path).ok());
                    let (text, fetched) = match cached {
                        Some(text) => {
                            let path = track
                                .lyrics
                                .as_ref()
                                .map(|path| path.display().to_string())
                                .unwrap_or_default();
                            eprintln!(
                                "[lyrics] \"{}\" — {} · cache file {path} · {} chars",
                                track.title,
                                track.artist,
                                text.len()
                            );
                            (Some(text), false)
                        }
                        None => {
                            let text = crate::ui::lyrics_store::load_or_fetch(
                                &track.title,
                                &track.artist,
                                Some(track.album.as_str()),
                                duration_secs(track.duration),
                            );
                            let fetched = text.is_some();
                            (text, fetched)
                        }
                    };
                    let parsed =
                        text.and_then(|text| lyrics::parse(&text, duration_ms(track.duration)));
                    (parsed, fetched)
                })
                .await;
            this.update(cx, |view, cx| {
                view.fullscreen.loading.remove(&id);
                if fetched {
                    view.session.update(cx, |session, cx| {
                        session.refresh_lyrics(id);
                        cx.notify();
                    });
                }
                let source = view
                    .session
                    .read(cx)
                    .track(id)
                    .and_then(|track| track.lyrics.clone());
                view.fullscreen.lyrics.insert(
                    id,
                    LyricsEntry {
                        lyrics: parsed.map(Arc::new),
                        stamp: stamp_of(source.as_deref()),
                        checked: Instant::now(),
                    },
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn ensure_backdrop(&mut self, cover: Option<PathBuf>, cx: &mut Context<Self>) {
        let backdrop = &mut self.fullscreen.backdrop;
        if backdrop.key == cover {
            return;
        }
        let Some(path) = cover else {
            backdrop.key = None;
            backdrop.previous = backdrop.current.take().map(|(image, _)| image);
            return;
        };
        if let Some(image) = backdrop.cache.get(&path).cloned() {
            let previous = backdrop.current.take().map(|(image, _)| image);
            if previous.is_some() {
                backdrop.previous = previous;
            }
            backdrop.current = image.map(|image| (image, Instant::now()));
            backdrop.key = Some(path);
            return;
        }
        if !backdrop.pending.insert(path.clone()) {
            return;
        }
        if backdrop.cache.len() > 48 {
            backdrop.cache.clear();
        }
        let source = path.clone();
        cx.spawn(async move |this, cx| {
            let image = cx
                .background_executor()
                .spawn(async move { blur_cover(&source) })
                .await;
            this.update(cx, |view, cx| {
                let backdrop = &mut view.fullscreen.backdrop;
                backdrop.pending.remove(&path);
                backdrop.cache.insert(path, image);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn full_cover(&mut self, thumb: &Path) -> PathBuf {
        if let Some(full) = self.fullscreen.full_covers.get(thumb) {
            return full.clone();
        }
        let full = thumb
            .to_str()
            .and_then(|path| path.strip_suffix("-thumb.webp"))
            .map(|stem| PathBuf::from(format!("{stem}-full.jpg")))
            .filter(|path| path.is_file())
            .unwrap_or_else(|| thumb.to_path_buf());
        self.fullscreen
            .full_covers
            .insert(thumb.to_path_buf(), full.clone());
        full
    }

    pub(crate) fn fullscreen_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.fullscreen.open {
            return None;
        }
        let now = Instant::now();
        let appear = self.fullscreen.appear(now);
        if self.fullscreen.closing_at.is_some() && appear <= 0.001 {
            self.fullscreen.open = false;
            self.fullscreen.closing_at = None;
            return None;
        }

        let snapshot = {
            let session = self.session.read(cx);
            session.current_track().cloned().map(|track| {
                let upcoming: Vec<TrackId> = session
                    .upcoming_ids(track.id)
                    .into_iter()
                    .filter(|id| *id != track.id)
                    .take(2)
                    .collect();
                let time_label = if self.show_remaining {
                    session::format_remaining(session.position, track.duration)
                } else {
                    session::format_time(session.position)
                };
                (
                    track,
                    upcoming,
                    session.position,
                    session.playing,
                    session.progress(),
                    time_label,
                    if session.shuffle {
                        Some(session.order_mode)
                    } else {
                        None
                    },
                    session.repeat,
                    session.volume,
                )
            })
        };
        let Some((track, upcoming, real, playing, progress, time_label, order, repeat, volume)) =
            snapshot
        else {
            self.close_fullscreen(cx);
            return None;
        };
        let id = track.id;
        let duration_label = session::format_time(track.duration);

        self.ensure_lyrics(id, cx);
        for next in upcoming {
            self.ensure_lyrics(next, cx);
        }
        self.ensure_backdrop(track.cover.clone(), cx);

        let fs = &mut self.fullscreen;
        let time = fs.clock.time(Some(id), real, playing);
        let dt = now.duration_since(fs.drift_at).as_secs_f64().min(0.1);
        fs.drift_at = now;
        if playing {
            fs.drift += dt;
        }
        let drift = fs.drift;
        let lyrics_now = fs.lyrics.get(&id).and_then(|entry| entry.lyrics.clone());
        let has_lyrics = lyrics_now.is_some();
        let show = fs.show_lyrics && has_lyrics;
        let show_toggle = fs.show_lyrics;
        let panel_mounted = has_lyrics && (show || fs.panel_toggled.elapsed() < PANEL_LINGER);
        let idle = now.duration_since(fs.last_move) > CHROME_IDLE;
        let chrome_on = !playing || !idle || fs.chrome_hover;
        let backdrop_fade = fs.backdrop.fade(now);
        let backdrop_current = fs.backdrop.current.as_ref().map(|(image, _)| image.clone());
        let backdrop_previous = fs.backdrop.previous.clone();
        if backdrop_fade >= 1.0 {
            fs.backdrop.previous = None;
        }

        let viewport = window.viewport_size();
        let vw = viewport.width.as_f32();
        let vh = viewport.height.as_f32();
        let area_h = (vh - TOP_BAR_H - BOTTOM_CHROME_H).max(200.);

        let (panel, panel_ready) = match (&lyrics_now, panel_mounted) {
            (Some(lyr), true) => {
                let px_scale = window.scale_factor();
                let (panel, ready) =
                    self.lyrics_panel(id, lyr.clone(), time, vw, px_scale, window, cx);
                (Some(panel), ready)
            }
            _ => (None, true),
        };
        let cover = self.fullscreen_cover(&track, area_h, vw, playing, cx);

        let caption = self.header_buttons(window, cx).into_any_element();
        let close_button = self.close_fullscreen_button(cx);
        let progress_el = self
            .progress_bar(progress, &time_label, &duration_label, cx)
            .into_any_element();
        let controls = self
            .transport_controls(playing, true, order, repeat, cx)
            .into_any_element();
        let toggle = self.glyph_button(
            "fs-lyrics",
            icons::LYRICS,
            Some((icons::LYRICS_OFF, !show_toggle)),
            34.,
            18.,
            if has_lyrics {
                theme::text()
            } else {
                theme::text_muted()
            },
            cx,
            |this, _, cx| {
                this.fullscreen.show_lyrics = !this.fullscreen.show_lyrics;
                this.fullscreen.panel_toggled = Instant::now();
                cx.notify();
            },
        );
        let volume_el = self.volume_control(volume, cx).into_any_element();

        let scroll_moving = self.fullscreen.scroll.animating(now);
        if appear < 1.0
            || playing
            || scroll_moving
            || backdrop_fade < 1.0
            || !panel_ready
            || self.fullscreen.closing_at.is_some()
        {
            window.request_animation_frame();
        }

        let mut backdrop_layers: Vec<AnyElement> = Vec::new();
        if let Some(previous) = backdrop_previous.filter(|_| backdrop_fade < 1.0) {
            backdrop_layers.extend(backdrop_layer(previous, 1.0, vw, vh, drift));
        }
        if let Some(current) = backdrop_current {
            backdrop_layers.extend(backdrop_layer(current, backdrop_fade, vw, vh, drift));
        }

        let content_shift = (1.0 - appear) * 28.0;
        let content = div()
            .absolute()
            .top(px((vh - area_h) / 2. + content_shift))
            .left(px(0.))
            .w(px(vw))
            .h(px(area_h))
            .with_spring(
                "fs-split",
                SpringAnimation::new(SpringConfig::new(120.0, 22.0, 1.0)).to(show),
                move |el, phase| {
                    let p = phase.0;
                    let shown = p.clamp(0.0, 1.0);
                    let cover_w = vw * (1.0 - 0.5 * p);
                    el.child(
                        div()
                            .absolute()
                            .top(px(0.))
                            .left(px(0.))
                            .h(px(area_h))
                            .w(px(cover_w))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(cover),
                    )
                    .when_some(panel, |el, panel| {
                        el.child(
                            div()
                                .absolute()
                                .top(px(0.))
                                .h(px(area_h))
                                .left(px(vw * 0.5 + PANEL_SLIDE_PX * (1.0 - p)))
                                .w(px((vw * 0.5 - 56.).max(200.)))
                                .opacity(shown)
                                .child(panel),
                        )
                    })
                },
            );

        let top_bar = div()
            .id("fs-top")
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .right(px(0.))
            .h(px(TOP_BAR_H))
            .px(px(8.))
            .flex()
            .items_center()
            .justify_end()
            .gap(px(6.))
            .window_control_area(WindowControlArea::Drag)
            .on_hover(cx.listener(|this, over: &bool, _, cx| {
                this.fullscreen.chrome_hover = *over;
                cx.notify();
            }))
            .child(close_button)
            .child(caption)
            .with_spring(
                "fs-top-chrome",
                Self::smooth_spring(chrome_on),
                |el, phase| el.opacity(phase.0.clamp(0.0, 1.0)),
            );

        let bottom = div()
            .id("fs-bottom")
            .absolute()
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .px(px(48.))
            .pt(px(16.))
            .pb(px(24.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .on_hover(cx.listener(|this, over: &bool, _, cx| {
                this.fullscreen.chrome_hover = *over;
                cx.notify();
            }))
            .child(progress_el)
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(div().flex().flex_1().items_center().child(toggle))
                    .child(controls)
                    .child(volume_el),
            )
            .with_spring(
                "fs-bottom-chrome",
                Self::smooth_spring(chrome_on),
                |el, phase| el.opacity(phase.0.clamp(0.0, 1.0)),
            );

        Some(
            div()
                .id("fs-root")
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .size_full()
                .overflow_hidden()
                .bg(theme::bg_deep())
                .opacity(appear)
                .on_mouse_move(cx.listener(|this, _: &gpui::MouseMoveEvent, _, cx| {
                    let was_idle = this.fullscreen.last_move.elapsed() > CHROME_IDLE;
                    this.fullscreen.last_move = Instant::now();
                    if was_idle {
                        cx.notify();
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _: &gpui::MouseDownEvent, _, cx| cx.stop_propagation()),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|_, _: &gpui::MouseDownEvent, _, cx| cx.stop_propagation()),
                )
                .on_scroll(cx.listener(|_, _: &gpui::ScrollEvent, _, cx| cx.stop_propagation()))
                .children(backdrop_layers)
                .child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(px(0.))
                        .size_full()
                        .bg(black_a(0.38)),
                )
                .child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(px(0.))
                        .size_full()
                        .bg(linear_gradient(
                            180.,
                            linear_color_stop(black_a(0.0), 0.55),
                            linear_color_stop(black_a(0.6), 1.0),
                        )),
                )
                .child(content)
                .child(top_bar)
                .child(bottom)
                .into_any_element(),
        )
    }

    fn close_fullscreen_button(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("fs-close")
            .flex()
            .items_center()
            .justify_center()
            .size(px(32.))
            .rounded_md()
            .cursor_pointer()
            .occlude()
            .child(icon(
                icons::CLOSE_FULLSCREEN,
                theme::text_secondary(),
                px(16.),
            ))
            .hover(|style| style.bg(white_a(0.1)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.arm("fs-close", false);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.armed("fs-close") {
                        this.close_fullscreen(cx);
                    }
                }),
            )
            .into_any_element()
    }

    fn fullscreen_cover(
        &mut self,
        track: &session::Track,
        area_h: f32,
        vw: f32,
        playing: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = track.id;
        let size = (area_h - 90.).min(vw * 0.36).clamp(160., 560.);
        let full = track.cover.as_deref().map(|thumb| self.full_cover(thumb));
        let title = track.title.clone();
        let artist = track.artist.clone();
        let liked = track.liked;
        let like = self.like_handlers(
            SharedString::from(format!("fs-like-{}", id.0)),
            id,
            false,
            cx,
        );
        let on_cover_menu =
            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.open_track_menu(id, event.position, window, cx);
            });
        let on_meta_menu =
            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.open_track_menu(id, event.position, window, cx);
            });
        // Pause scales this column as one card: cover, title, artist, and like.
        div()
            .flex()
            .flex_col()
            .items_center()
            .child(div().with_spring(
                "fs-cover-pause",
                SpringAnimation::new(SpringConfig::new(170.0, 22.0, 1.0)).to(playing),
                move |card, phase| {
                    let s = pause_factor(phase.0);
                    let box_size = size * s;
                    let radius = 12. * s;
                    let picture = match &full {
                        Some(path) => img(path.clone())
                            .size(px(box_size))
                            .rounded(px(radius))
                            .object_fit(ObjectFit::Cover)
                            .into_any_element(),
                        None => cover_art(None, box_size),
                    };
                    card.flex()
                        .flex_col()
                        .items_center()
                        .w(px(box_size))
                        .child(
                            div()
                                .id("fs-cover")
                                .size(px(box_size))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(radius))
                                .shadow(vec![BoxShadow {
                                    color: black_a(0.45),
                                    offset: point(px(0.), px(18. * s)),
                                    blur_radius: px(48. * s),
                                    spread_radius: px(0.),
                                    inset: false,
                                }])
                                .on_mouse_down(MouseButton::Right, on_cover_menu)
                                .child(picture),
                        )
                        .child(
                            div()
                                .w(px(box_size))
                                .mt(px(22. * s))
                                .flex()
                                .items_center()
                                .gap(px(12. * s))
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .on_mouse_down(MouseButton::Right, on_meta_menu)
                                        .child(
                                            div()
                                                .text_size(px(26. * s))
                                                .line_height(px(32. * s))
                                                .font_weight(gpui::FontWeight::BOLD)
                                                .text_color(theme::white())
                                                .truncate()
                                                .child(title.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(16. * s))
                                                .line_height(px(22. * s))
                                                .text_color(white_a(0.6))
                                                .truncate()
                                                .child(artist.clone()),
                                        ),
                                )
                                .child(like_face(like, liked, 40. * s, 22. * s, theme::text(), false)),
                        )
                },
            ))
            .into_any_element()
    }

    fn lyrics_panel(
        &mut self,
        id: TrackId,
        lyr: Arc<Lyrics>,
        time: f64,
        vw: f32,
        px_scale: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, bool) {
        let untimed = lyr.sync == SyncType::None;
        let lines = &lyr.lines;
        let lead_ms = if lyr.sync == SyncType::Rich {
            lyric_motion::RICH_LEAD_MS
        } else {
            lyric_motion::LINE_LEAD_MS
        };
        let now_ms = time * 1000.0 + lead_ms;
        let scroll_time = time + lead_ms as f64 / 1000.0 + lyric_motion::SCROLL_LEAD_SEC;
        let current = lyrics::active_lines(lines, now_ms / 1000.0);
        let offsets = self
            .fullscreen
            .line_offsets(id, lines, scroll_time, untimed);
        let ready = offsets.is_some();
        let offsets = offsets.unwrap_or_else(|| vec![0.; lines.len()]);
        let (tops, heights, view_h) = {
            let m = self.fullscreen.measure.borrow();
            (m.tops.clone(), m.heights.clone(), m.view_h)
        };
        let font = (vw * 0.026).clamp(26., 44.);
        let pulse = 0.82 + 0.18 * ((time * 3.0).sin() as f32 * 0.5 + 0.5);
        let measure = self.fullscreen.measure.clone();

        let key = (id, Arc::as_ptr(&lyr) as usize);
        let mut groups = std::mem::take(&mut self.fullscreen.groups);
        if groups.key != Some(key) {
            groups.key = Some(key);
            groups.lines = lines
                .iter()
                .map(|line| line_groups(line, lyr.sync))
                .collect();
            groups.wrap_key = None;
        }
        let text_w = ((vw * 0.5 - 56.).max(200.) - 28.).max(80.);
        let wrap_key = ((font * 10.0).round() as i32, (text_w * 2.0).round() as i32);
        if groups.wrap_key != Some(wrap_key) || groups.wraps.len() != groups.lines.len() {
            groups.wraps = groups
                .lines
                .iter()
                .map(|(main, back)| LineWrap {
                    main: measure_rows(window, main, font, gpui::FontWeight::BOLD, text_w),
                    back: measure_rows(
                        window,
                        back,
                        font * BACKGROUND_SCALE,
                        gpui::FontWeight::MEDIUM,
                        text_w,
                    ),
                })
                .collect();
            groups.wrap_key = Some(wrap_key);
        }

        let rich = lyr.sync == SyncType::Rich;
        let sung_anchor = if current.is_empty() {
            lines
                .iter()
                .enumerate()
                .rev()
                .find(|(_, line)| now_ms >= line.start_ms as f64)
                .map(|(index, _)| index)
        } else {
            None
        };
        let wall = Instant::now();
        if self.fullscreen.scale_track != Some(id) {
            self.fullscreen.scale_track = Some(id);
            self.fullscreen.scales.clear();
        }
        let mut rows: Vec<AnyElement> = Vec::with_capacity(lines.len());
        for (index, line) in lines.iter().enumerate() {
            let offset = offsets[index];
            let center = tops.get(index).copied().unwrap_or(0.)
                + heights.get(index).copied().unwrap_or(0.) / 2.
                - offset;
            let edge = if view_h > 0.0 {
                smoothstep(0.0, view_h * 0.14, center.min(view_h - center))
            } else {
                1.0
            };
            let centered = line.agent.as_deref() == Some("v1000");
            let right = !centered && matches!(line.agent.as_deref(), Some("v2") | Some("v3"));
            let seekable = !untimed;
            let seek = line.start_ms as f64 / 1000.0;
            let arm_id = SharedString::from(format!("fs-lyric-{index}"));
            let release_id = arm_id.clone();
            let probe = {
                let measure = measure.clone();
                canvas(
                    move |bounds, window, _| {
                        let mut m = measure.borrow_mut();
                        if m.tops.len() <= index {
                            m.tops.resize(index + 1, 0.);
                            m.heights.resize(index + 1, 0.);
                        }
                        let applied = m.applied.get(index).copied().unwrap_or(0.);
                        let natural = bounds.origin.y.as_f32() - m.view_top + applied;
                        let height = bounds.size.height.as_f32();
                        if (m.tops[index] - natural).abs() > 2.0
                            || (m.heights[index] - height).abs() > 2.0
                        {
                            m.tops[index] = natural;
                            m.heights[index] = height;
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .right(px(0.))
                .bottom(px(0.))
            };
            let align = if centered {
                Align::Center
            } else if right {
                Align::End
            } else {
                Align::Start
            };
            let distance = if untimed {
                0
            } else if !current.is_empty() {
                current
                    .iter()
                    .map(|active| (index as i32 - *active as i32).abs())
                    .min()
                    .unwrap_or(0)
            } else {
                sung_anchor
                    .map(|anchor| (index as i32 - anchor as i32).abs())
                    .unwrap_or(2)
            };
            let (depth_opacity, size_mult) = line_depth(distance);
            // Far lines stay on the full layout box. Only the paint shrinks.
            // The target flips on a click; the shown scale eases over the same
            // 166ms the playback grow uses, so the line does not pop.
            let grow_end = {
                let own =
                    line.start_ms as f64 + line.duration_ms as f64 + lyric_motion::LINE_SETTLE_MS;
                lines
                    .get(index + 1)
                    .map(|next| (next.start_ms as f64).max(own))
                    .unwrap_or(own)
            };
            let grown = if untimed || (now_ms >= line.start_ms as f64 && now_ms < grow_end) {
                1.0
            } else {
                lyric_motion::LINE_REST_SCALE
            };
            let paint = self
                .fullscreen
                .step_scale(index, grown * size_mult, wall);
            let content: AnyElement = if untimed {
                div()
                    .text_color(white_a(1.0))
                    .child(line.words.clone())
                    .into_any_element()
            } else if line.instrumental {
                let progress = lyrics::break_progress(lines, index, time);
                let active = current.contains(&index);
                let dot_font = font * paint;
                div()
                    .w_full()
                    .min_h(px(font * 1.15))
                    .flex()
                    .items_center()
                    .when(matches!(align, Align::End), |el| el.justify_end())
                    .when(matches!(align, Align::Center), |el| el.justify_center())
                    .when(matches!(align, Align::Start), |el| el.justify_start())
                    .with_spring(
                        SharedString::from(format!("fs-line-{}-{index}", id.0)),
                        SpringAnimation::new(SpringConfig::new(170.0, 26.0, 1.0)).to(active),
                        move |el, phase| {
                            let phase = phase.0.clamp(0.0, 1.0);
                            el.child(break_dots(progress, phase, dot_font, pulse))
                        },
                    )
                    .into_any_element()
            } else {
                let start_ms = line
                    .parts
                    .iter()
                    .filter(|part| !part.background)
                    .map(|part| part.start_ms)
                    .min()
                    .unwrap_or(line.start_ms)
                    .min(line.start_ms) as f64;
                let end_ms = (lyrics::line_until_sec(lines, index) * 1000.0)
                    .max((line.start_ms + line.duration_ms) as f64 + lyric_motion::LINE_SETTLE_MS);
                let motion = lyric_motion::line_motion(start_ms, end_ms, now_ms);
                let live = !motion.still;
                let mut motion = motion;
                if motion.still && !motion.past && now_ms >= start_ms - PRE_ROLL_MS {
                    motion.still = false;
                }
                let view = LineView {
                    motion,
                    live,
                    now_ms,
                    // Rich sync keeps unsung words gray. Line sync stays at the
                    // neighbor ink so the brighten does not flash darker first.
                    base: line_ink(distance, rich),
                    px_scale,
                    detailed: !motion.still,
                };
                let (main, back) = &groups.lines[index];
                let wrap = &groups.wraps[index];
                let full = lyric_line(main, back, &wrap.main, &wrap.back, view, font, align, rich, window);
                if (paint - 1.0).abs() < 0.004 {
                    full
                } else {
                    resting_scale(
                        full,
                        lyric_line(
                            main,
                            back,
                            &wrap.main,
                            &wrap.back,
                            view,
                            font * paint,
                            align,
                            rich,
                            window,
                        ),
                        align,
                        font * paint,
                    )
                }
            };
            let row_opacity = if !untimed && line.instrumental {
                edge * depth_opacity
            } else {
                edge
            };
            let row = div()
                .id(arm_id.clone())
                .relative()
                .top(px(-offset))
                .w_full()
                .px(px(14.))
                .py(px(LINE_PAD))
                .rounded(px(14.))
                .opacity(row_opacity)
                .when(right, |row| row.text_right())
                .when(centered, |row| row.text_center())
                .child(probe)
                .child(content)
                .when(seekable, |row| {
                    row.cursor_pointer()
                        .hover(|style| style.bg(white_a(0.06)))
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
                                this.fullscreen.scroll.user = None;
                                this.fullscreen.scroll.force = true;
                                this.fullscreen.clock.reset();
                                this.session.update(cx, |session, cx| {
                                    session.seek_to(seek);
                                    cx.notify();
                                });
                                cx.notify();
                            }),
                        )
                });
            rows.push(row.into_any_element());
        }
        self.fullscreen.groups = groups;

        let viewport_probe = {
            let measure = measure.clone();
            canvas(
                move |bounds, window, _| {
                    let mut m = measure.borrow_mut();
                    m.view_top = bounds.origin.y.as_f32();
                    let height = bounds.size.height.as_f32();
                    if (m.view_h - height).abs() > 2.0 {
                        m.view_h = height;
                        window.request_animation_frame();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
        };

        let panel = div()
            .id("fs-lyrics")
            .relative()
            .size_full()
            .overflow_hidden()
            .on_scroll(cx.listener(move |this, event: &gpui::ScrollEvent, _, cx| {
                cx.stop_propagation();
                let delta = match event.delta {
                    gpui::ScrollDelta::Lines(lines) => lines.y * 48.,
                    gpui::ScrollDelta::Pixels(pixels) => pixels.y.as_f32(),
                };
                if delta == 0.0 {
                    return;
                }
                this.fullscreen.wheel(delta, untimed);
                cx.notify();
            }))
            .child(viewport_probe)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .pt(px(COLUMN_TOP_PAD))
                    .text_size(px(font))
                    .line_height(px(font * LINE_HEIGHT))
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(theme::white())
                    .opacity(if ready { 1.0 } else { 0.0 })
                    .children(rows),
            )
            .into_any_element();
        (panel, ready)
    }
}

fn backdrop_layer(image: Arc<Image>, alpha: f32, vw: f32, vh: f32, drift: f64) -> Vec<AnyElement> {
    let t = drift as f32;
    let layer = |scale: f32, speed: f32, phase: f32, opacity: f32| {
        let side = vw.max(vh) * scale;
        let dx = (t * speed + phase).sin() * vw * 0.08;
        let dy = (t * speed * 0.8 + phase * 1.7).cos() * vh * 0.08;
        div()
            .absolute()
            .left(px((vw - side) / 2. + dx))
            .top(px((vh - side) / 2. + dy))
            .size(px(side))
            .opacity(alpha * opacity)
            .child(
                img(image.clone())
                    .size(px(side))
                    .object_fit(ObjectFit::Cover),
            )
            .into_any_element()
    };
    vec![layer(1.5, 0.05, 0.0, 1.0), layer(1.9, 0.035, 2.4, 0.55)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(text: &str, duration_ms: f32) -> Part {
        Part {
            text: text.into(),
            start_ms: 0.0,
            duration_ms,
        }
    }

    #[test]
    fn syllables_join_into_words_at_spaces() {
        let groups = groups_of(vec![
            part("Hel", 200.),
            part("lo ", 900.),
            part("world", 300.),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].parts.len(), 2);
        assert_eq!(groups[0].parts[1].text, "lo");
        assert!(groups[0].space_after);
        assert!(!groups[1].space_after);
    }

    #[test]
    fn leading_space_starts_a_new_word() {
        let groups = groups_of(vec![part("Привет", 100.), part(" мир", 100.)]);
        assert_eq!(groups.len(), 2);
        assert!(groups[0].space_after);
    }

    #[test]
    fn cjk_syllables_stay_separate_so_lines_wrap() {
        let groups = groups_of(vec![part("夜", 100.), part("に", 100.), part("駆", 100.)]);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|group| !group.space_after));
    }

    #[test]
    fn line_synced_text_splits_into_words() {
        let groups = groups_of(line_parts("Hello big 世界", 0.0));
        let words: Vec<String> = groups
            .iter()
            .map(|group| group.parts.iter().map(|part| part.text.as_str()).collect())
            .collect();
        assert_eq!(words, vec!["Hello", "big", "世", "界"]);
    }

    #[test]
    fn letters_keep_marks_and_joined_emoji() {
        assert_eq!(letters_of("й").len(), 1);
        assert_eq!(letters_of("e\u{301}a").len(), 2);
        assert_eq!(letters_of("👩\u{200D}🎤!").len(), 2);
        assert!(splittable("Привет"));
        assert!(!splittable("مرحبا"));
    }

    #[test]
    fn a_late_position_sample_does_not_rewind_the_lyric_clock() {
        let mut clock = Clock::default();
        let start = clock.time(Some(TrackId(1)), 10.0, true);
        assert!((start - 10.0).abs() < 1e-9);
        let held = clock.time(Some(TrackId(1)), 9.98, true);
        assert!(held > 9.99, "{held}");
        let seek = clock.time(Some(TrackId(1)), 30.0, true);
        assert!((seek - 30.0).abs() < 1e-9);
    }

    #[test]
    fn words_wrap_before_they_are_painted() {
        let widths = [30.0, 30.0, 30.0, 80.0];
        let gaps = [0.0, 8.0, 8.0, 8.0];
        let rows = pack_rows(&widths, &gaps, 80.0);
        assert_eq!(rows, vec![vec![0, 1], vec![2], vec![3]]);
        let wide = pack_rows(&[200.0, 10.0], &[0.0, 4.0], 80.0);
        assert_eq!(wide, vec![vec![0], vec![1]]);
        assert_eq!(pack_rows(&[], &[], 80.0), Vec::<Vec<usize>>::new());
    }

    #[test]
    fn line_synced_words_do_not_wobble() {
        let rich = group_shift(false, true, 60.0, 40.0, 1.0, 1.0);
        let line = group_shift(false, false, 60.0, 40.0, 1.0, 1.0);
        assert!(rich.abs() > 0.0);
        assert_eq!(line, 0.0);
        assert_eq!(group_shift(true, true, 60.0, 40.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn lines_farther_from_the_sung_one_are_smaller_and_dimmer() {
        assert_eq!(line_depth(0), (1.0, 1.0));
        assert_eq!(line_depth(1), (0.45, 0.99));
        assert_eq!(line_depth(2), (0.3, 0.98));
        assert_eq!(line_depth(5), (0.3, 0.98));
    }

    #[test]
    fn a_line_synced_line_does_not_dip_when_it_brightens() {
        let neighbor = line_ink(1, false);
        let active = line_ink(0, false);
        assert_eq!(active, neighbor);
        assert!(active > line_ink(0, true));
    }

    #[test]
    fn a_clicked_line_eases_to_its_new_size() {
        let start = Instant::now();
        let anim = ScaleAnim {
            from: lyric_motion::LINE_REST_SCALE,
            to: 1.0,
            started: start,
        };
        assert!((scale_value(&anim, start) - lyric_motion::LINE_REST_SCALE).abs() < 1e-4);
        let mid = start + Duration::from_millis((lyric_motion::LINE_SCALE_MS * 0.5) as u64);
        let at = scale_value(&anim, mid);
        assert!(at > lyric_motion::LINE_REST_SCALE && at < 1.0);
        let done = start + Duration::from_millis(lyric_motion::LINE_SCALE_MS as u64 + 5);
        assert!((scale_value(&anim, done) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn a_scroll_commit_holds_the_picture_still() {
        let now = Instant::now();
        let delta = 120.0;
        let glide = Glide::rest(500.0 + delta);
        let shift = Shift {
            from: delta,
            start: now,
            dur: 0.8,
        };
        let visual = glide.pos(now) - shift.value(now);
        assert!((visual - 500.0).abs() < 0.01);
    }
}
