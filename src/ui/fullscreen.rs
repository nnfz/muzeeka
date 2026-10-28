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

const APPEAR_SEC: f32 = 0.48;
const CLOSE_SEC: f32 = 0.32;
const CHROME_IDLE: Duration = Duration::from_millis(1800);
const PANEL_LINGER: Duration = Duration::from_millis(900);
const PANEL_SLIDE_PX: f32 = 196.;
const BACKDROP_FADE_SEC: f32 = 0.7;
const TOP_BAR_H: f32 = 48.;
const BOTTOM_CHROME_H: f32 = 128.;

const STAGGER_SEC: f32 = 0.045;
const STAGGER_MAX_SEC: f32 = 0.4;
const LINE_MOVE_SEC: f32 = 0.62;
const WHEEL_MOVE_SEC: f32 = 0.28;
const USER_HOLD: Duration = Duration::from_secs(3);
const ANCHOR_FRACTION: f32 = 0.5;
const COLUMN_TOP_PAD: f32 = 80.;

const PAST_ALPHA: f32 = 0.18;
const UPCOMING_ALPHA: f32 = 0.26;
const ACTIVE_ALPHA: f32 = 0.4;
const LINE_HEIGHT: f32 = 1.2;
const WIPE_BANDS: usize = 4;
const LONG_GROUP_CHARS: usize = 20;
const BACKGROUND_SCALE: f32 = 0.75;
const BACKGROUND_DIM: f32 = 0.78;
const GLOW_RADIUS_EM: f32 = 0.45;
const GLOW_ALPHA: f32 = 0.14;
const PRE_ROLL_MS: f64 = 1000.0;

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
}

#[derive(Default)]
struct GroupCache {
    key: Option<(TrackId, usize)>,
    lines: Vec<(Vec<Group>, Vec<Group>)>,
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
        }
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
        count: usize,
        untimed: bool,
        anchor: Option<usize>,
    ) -> Option<Vec<f32>> {
        let now = Instant::now();
        let measure = self.measure.clone();
        let mut m = measure.borrow_mut();
        let s = &mut self.scroll;
        if s.track != Some(track) || s.count != count {
            *s = LyricScroll {
                track: Some(track),
                count,
                ..Default::default()
            };
            m.tops = vec![0.; count];
            m.heights = vec![0.; count];
        }
        if !m.valid(count) {
            m.applied = vec![0.; count];
            return None;
        }
        let goal = auto_target(&m, anchor, untimed);
        let anchor_index = anchor.unwrap_or(0);
        if s.motions.len() != count {
            s.motions = vec![Motion::rest(goal); count];
            s.target = goal;
            s.anchor = anchor;
        } else if let Some(at) = s.user {
            if !untimed && now.duration_since(at) > USER_HOLD {
                s.user = None;
                s.anchor = anchor;
                s.force = false;
                s.retarget(goal, anchor_index, now);
            }
        } else if s.anchor != anchor || s.force {
            s.anchor = anchor;
            s.force = false;
            s.retarget(goal, anchor_index, now);
        } else if (goal - s.target).abs() > 0.5 {
            let delta = goal - s.target;
            for motion in &mut s.motions {
                motion.from += delta;
                motion.to += delta;
            }
            s.target = goal;
        }
        let values: Vec<f32> = s.motions.iter().map(|motion| motion.value(now)).collect();
        m.applied = values.clone();
        Some(values)
    }

    fn wheel(&mut self, delta: f32, untimed: bool) {
        let m = self.measure.borrow();
        let count = m.tops.len();
        if !m.valid(count) || self.scroll.motions.len() != count {
            return;
        }
        let now = Instant::now();
        let (lo, hi) = scroll_range(&m, untimed);
        let s = &mut self.scroll;
        let next = (s.target - delta).clamp(lo, hi);
        for motion in &mut s.motions {
            *motion = Motion {
                from: motion.value(now),
                to: next,
                start: now,
                delay: 0.0,
                dur: WHEEL_MOVE_SEC,
            };
        }
        s.target = next;
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
    real_at: Option<Instant>,
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
        let snap = self.at.is_none() || self.track != track || self.playing != playing;
        if snap || real != self.real {
            self.real = real;
            self.real_at = Some(now);
        }
        let real_age = self
            .real_at
            .map(|at| now.duration_since(at).as_secs_f64().min(0.5))
            .unwrap_or(0.0);
        let target = if playing { real + real_age } else { real };
        if snap {
            self.shown = target;
        } else {
            let dt = self
                .at
                .map(|at| now.duration_since(at).as_secs_f64().min(0.5))
                .unwrap_or(0.0);
            let predicted = if playing { self.shown + dt } else { target };
            let error = target - predicted;
            self.shown = if error.abs() > 0.25 {
                target
            } else {
                predicted + error * (1.0 - (-dt * 6.0).exp())
            };
        }
        self.track = track;
        self.playing = playing;
        self.at = Some(now);
        self.shown
    }
}

#[derive(Clone, Copy)]
struct Motion {
    from: f32,
    to: f32,
    start: Instant,
    delay: f32,
    dur: f32,
}

impl Motion {
    fn rest(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            delay: 0.0,
            dur: 0.0,
        }
    }

    fn value(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.start).as_secs_f32() - self.delay;
        if self.dur <= 0.0 || t >= self.dur {
            return self.to;
        }
        if t <= 0.0 {
            return self.from;
        }
        self.from + (self.to - self.from) * ease_out_cubic(t / self.dur)
    }

    fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.start).as_secs_f32() >= self.delay + self.dur
    }
}

#[derive(Default)]
struct LyricScroll {
    track: Option<TrackId>,
    count: usize,
    motions: Vec<Motion>,
    target: f32,
    anchor: Option<usize>,
    user: Option<Instant>,
    force: bool,
}

impl LyricScroll {
    fn retarget(&mut self, goal: f32, anchor: usize, now: Instant) {
        let forward = goal >= self.target;
        for (index, motion) in self.motions.iter_mut().enumerate() {
            let rank = if forward {
                index.saturating_sub(anchor)
            } else {
                anchor.saturating_sub(index)
            };
            let delay = (rank as f32 * STAGGER_SEC).min(STAGGER_MAX_SEC);
            *motion = Motion {
                from: motion.value(now),
                to: goal,
                start: now,
                delay,
                dur: LINE_MOVE_SEC,
            };
        }
        self.target = goal;
    }

    fn animating(&self, now: Instant) -> bool {
        self.motions.iter().any(|motion| !motion.done(now))
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

fn auto_target(m: &Measure, anchor: Option<usize>, untimed: bool) -> f32 {
    match anchor {
        Some(index) if !untimed && index < m.tops.len() => {
            m.tops[index] + m.heights[index] / 2.0 - m.view_h * ANCHOR_FRACTION
        }
        _ => 0.0,
    }
}

fn scroll_range(m: &Measure, untimed: bool) -> (f32, f32) {
    let content = m.content_h();
    if untimed {
        (0.0, (content - m.view_h * 0.7).max(0.0))
    } else {
        let lo = m.tops.first().copied().unwrap_or(0.0) - m.view_h * ANCHOR_FRACTION;
        (lo, (content - m.view_h * ANCHOR_FRACTION).max(lo))
    }
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
    now_ms: f64,
    base: f32,
    px_scale: f32,
    detailed: bool,
}

struct Unit {
    text: String,
    lit: Lit,
    highlight: f32,
    glow: f32,
    lift: f32,
    rtl: bool,
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

fn line_groups(line: &LyricLine, sync: SyncType) -> (Vec<Group>, Vec<Group>) {
    if sync == SyncType::Rich && !line.parts.is_empty() {
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
        return (groups_of(main), groups_of(back));
    }
    (
        groups_of(line_parts(&line.words, line.start_ms as f64)),
        Vec::new(),
    )
}

fn part_units(part: &Part, view: LineView, font: f32) -> Vec<Unit> {
    let motion = view.motion;
    let rtl = is_rtl(&part.text);
    let since = (view.now_ms - part.start_ms) as f32;
    let fade = if motion.still { 0.0 } else { motion.highlight };
    let glow = if motion.past || motion.still {
        0.0
    } else {
        lyric_motion::glow(since, part.duration_ms)
    };
    let whole = |lit: Lit, highlight: f32| {
        vec![Unit {
            text: part.text.clone(),
            lit,
            highlight,
            glow,
            lift: 0.0,
            rtl,
        }]
    };
    if part.duration_ms <= 0.0 {
        return whole(Lit::FULL, lyric_motion::line_synced_fade(since) * fade);
    }
    let highlight = if since >= 0.0 { fade } else { 0.0 };
    let start = lyric_motion::swipe_start(since, part.duration_ms);
    if !splittable(&part.text) {
        return whole(lyric_motion::word_lit(start), highlight);
    }
    let letters = letters_of(&part.text);
    let count = letters.len();
    let waves = lyric_motion::letter_waves(count, part.duration_ms);
    letters
        .into_iter()
        .zip(waves)
        .enumerate()
        .map(|(index, (text, wave))| Unit {
            text,
            lit: lyric_motion::letter_lit(start, index, count),
            highlight,
            glow,
            lift: snap(
                lyric_motion::float(since, wave) * font * fade,
                view.px_scale,
                1.0,
            ),
            rtl,
        })
        .collect()
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
    let over = |alpha: f32| alpha + base * (1.0 - alpha);
    if unit.highlight <= 0.0 || unit.lit.dark() {
        return div().text_color(white_a(base)).child(text);
    }
    if unit.lit.full() {
        return div().text_color(white_a(over(unit.highlight))).child(text);
    }
    let lo = unit.lit.edge.clamp(0.0, 1.0);
    let hi = (unit.lit.edge + unit.lit.fade).clamp(0.0, 1.0);
    let mut layers: Vec<AnyElement> = Vec::new();
    let mut push = |from: f32, to: f32, alpha: f32| {
        let (from, to) = if unit.rtl {
            (1.0 - to, 1.0 - from)
        } else {
            (from, to)
        };
        layers.extend(clip_layer(&text, from, to, white_a(alpha)));
    };
    push(0.0, lo, over(unit.highlight));
    for band in 0..WIPE_BANDS {
        let from = lo + (hi - lo) * band as f32 / WIPE_BANDS as f32;
        let to = lo + (hi - lo) * (band + 1) as f32 / WIPE_BANDS as f32;
        push(
            from,
            to,
            over(unit.lit.at((from + to) * 0.5) * unit.highlight),
        );
    }
    push(hi, 1.0, base);
    div()
        .relative()
        .child(div().text_color(white_a(0.0)).child(text))
        .children(layers)
}

fn unit_glow(unit: &Unit, font: f32) -> Option<AnyElement> {
    let amount = unit.lit.amount();
    let strength = unit.glow * unit.highlight * amount;
    if strength <= 0.01 {
        return None;
    }
    let left = if unit.rtl { 1.0 - amount } else { 0.0 };
    Some(
        div()
            .absolute()
            .top(px(font * 0.22))
            .bottom(px(font * 0.22))
            .left(relative(left))
            .w(relative(amount))
            .rounded(px(font * 0.3))
            .shadow(vec![BoxShadow {
                color: white_a(GLOW_ALPHA * strength),
                offset: point(px(0.), px(0.)),
                blur_radius: px(font * GLOW_RADIUS_EM * unit.glow),
                spread_radius: px(0.),
                inset: false,
            }])
            .into_any_element(),
    )
}

fn unit_element(unit: Unit, font: f32, base: f32) -> AnyElement {
    let glow = unit_glow(&unit, font);
    if glow.is_none() && unit.lift == 0.0 {
        return unit_text(&unit, base)
            .whitespace_nowrap()
            .into_any_element();
    }
    div()
        .relative()
        .top(px(-unit.lift))
        .whitespace_nowrap()
        .children(glow)
        .child(unit_text(&unit, base))
        .into_any_element()
}

fn group_element(group: &Group, view: LineView, font: f32) -> AnyElement {
    let chars: usize = group
        .parts
        .iter()
        .map(|part| part.text.chars().count())
        .sum();
    if !view.detailed {
        let text: String = group.parts.iter().map(|part| part.text.as_str()).collect();
        return div()
            .text_color(white_a(view.base))
            .when(chars > LONG_GROUP_CHARS, |el| el.max_w_full())
            .when(chars <= LONG_GROUP_CHARS, |el| el.whitespace_nowrap())
            .when(group.space_after, |el| el.mr(px(font * 0.26)))
            .child(text)
            .into_any_element();
    }
    let motion = view.motion;
    let first_ms = group.parts.first().map_or(0.0, |part| part.start_ms);
    let shift = if motion.still {
        0.0
    } else {
        let em = lyric_motion::wobble((view.now_ms - first_ms) as f32);
        snap(em * font * motion.highlight, view.px_scale, 4.0)
    };
    let units = group
        .parts
        .iter()
        .flat_map(|part| part_units(part, view, font))
        .map(|unit| unit_element(unit, font, view.base));
    div()
        .relative()
        .left(px(shift))
        .flex()
        .flex_row()
        .when(chars > LONG_GROUP_CHARS, |el| el.flex_wrap().max_w_full())
        .when(group.space_after, |el| el.mr(px(font * 0.26)))
        .children(units)
        .into_any_element()
}

fn word_row(groups: &[Group], view: LineView, font: f32, align: Align) -> gpui::Div {
    let row = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .children(groups.iter().map(|group| group_element(group, view, font)));
    match align {
        Align::Start => row.justify_start(),
        Align::Center => row.justify_center(),
        Align::End => row.justify_end(),
    }
}

fn lyric_line(
    main: &[Group],
    back: &[Group],
    view: LineView,
    font: f32,
    align: Align,
) -> AnyElement {
    let back_font = font * BACKGROUND_SCALE;
    let back_view = LineView {
        base: view.base * BACKGROUND_DIM,
        ..view
    };
    div()
        .flex()
        .flex_col()
        .when(!main.is_empty(), |column| {
            column.child(word_row(main, view, font, align))
        })
        .when(!back.is_empty(), |column| {
            column.child(
                word_row(back, back_view, back_font, align)
                    .mt(px(font * 0.1))
                    .text_size(px(back_font))
                    .line_height(px(back_font * LINE_HEIGHT))
                    .font_weight(gpui::FontWeight::MEDIUM),
            )
        })
        .into_any_element()
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
                        Some(text) => (Some(text), false),
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
                let (panel, ready) = self.lyrics_panel(id, lyr.clone(), time, vw, px_scale, cx);
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
        let art = div()
            .id("fs-cover")
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_track_menu(id, event.position, window, cx);
                }),
            )
            .child(div().with_spring(
                "fs-cover-pause",
                SpringAnimation::new(SpringConfig::new(170.0, 22.0, 1.0)).to(playing),
                move |el, phase| {
                    let scaled = size * (0.88 + 0.12 * phase.0);
                    let picture = match full {
                        Some(path) => img(path)
                            .size(px(scaled))
                            .rounded(px(12.))
                            .object_fit(ObjectFit::Cover)
                            .into_any_element(),
                        None => cover_art(None, scaled),
                    };
                    el.size(px(scaled))
                        .rounded(px(12.))
                        .shadow(vec![BoxShadow {
                            color: black_a(0.45),
                            offset: point(px(0.), px(18.)),
                            blur_radius: px(48.),
                            spread_radius: px(0.),
                            inset: false,
                        }])
                        .child(picture)
                },
            ));
        let like = self.like_button(
            SharedString::from(format!("fs-like-{}", id.0)),
            track.liked,
            id,
            40.,
            22.,
            theme::text(),
            false,
            cx,
        );
        div()
            .flex()
            .flex_col()
            .items_center()
            .child(art)
            .child(
                div()
                    .w(px(size))
                    .mt(px(22.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(26.))
                                    .line_height(px(32.))
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(theme::white())
                                    .truncate()
                                    .child(track.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .line_height(px(22.))
                                    .text_color(white_a(0.6))
                                    .truncate()
                                    .child(track.artist.clone()),
                            ),
                    )
                    .child(like),
            )
            .into_any_element()
    }

    fn lyrics_panel(
        &mut self,
        id: TrackId,
        lyr: Arc<Lyrics>,
        time: f64,
        vw: f32,
        px_scale: f32,
        cx: &mut Context<Self>,
    ) -> (AnyElement, bool) {
        let untimed = lyr.sync == SyncType::None;
        let lines = &lyr.lines;
        let anchor = if untimed {
            None
        } else {
            lyrics::scroll_anchor(lines, time + lyrics::LINE_LEAD_SEC)
        };
        let lead_ms = if lyr.sync == SyncType::Rich {
            lyric_motion::RICH_LEAD_MS
        } else {
            lyric_motion::LINE_LEAD_MS
        };
        let now_ms = time * 1000.0 + lead_ms;
        let current = lyrics::active_lines(lines, now_ms / 1000.0);
        let offsets = self
            .fullscreen
            .line_offsets(id, lines.len(), untimed, anchor);
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
            let right = matches!(line.agent.as_deref(), Some("v2") | Some("v3"));
            let centered = line.agent.as_deref() == Some("v1000");
            let seekable = !untimed && !line.instrumental;
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
                        if (m.tops[index] - natural).abs() > 0.5
                            || (m.heights[index] - height).abs() > 0.5
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
            let align = if right {
                Align::End
            } else if centered {
                Align::Center
            } else {
                Align::Start
            };
            let content: AnyElement = if untimed {
                div()
                    .text_color(white_a(1.0))
                    .child(line.words.clone())
                    .into_any_element()
            } else if line.instrumental {
                let progress = lyrics::break_progress(lines, index, time);
                let active = current.contains(&index);
                div()
                    .with_spring(
                        SharedString::from(format!("fs-line-{}-{index}", id.0)),
                        SpringAnimation::new(SpringConfig::new(170.0, 26.0, 1.0)).to(active),
                        move |el, phase| {
                            let phase = phase.0.clamp(0.0, 1.0);
                            el.child(break_dots(progress, phase, font, pulse))
                        },
                    )
                    .into_any_element()
            } else {
                let start_ms = line
                    .parts
                    .iter()
                    .map(|part| part.start_ms)
                    .min()
                    .unwrap_or(line.start_ms)
                    .min(line.start_ms) as f64;
                let end_ms = (lyrics::line_until_sec(lines, index) * 1000.0)
                    .max((line.start_ms + line.duration_ms) as f64 + lyric_motion::LINE_SETTLE_MS);
                let mut motion = lyric_motion::line_motion(start_ms, end_ms, now_ms);
                if motion.still && !motion.past && now_ms >= start_ms - PRE_ROLL_MS {
                    motion.still = false;
                }
                let rest = if motion.past {
                    PAST_ALPHA
                } else {
                    UPCOMING_ALPHA
                };
                let view = LineView {
                    motion,
                    now_ms,
                    base: rest + (ACTIVE_ALPHA - rest) * motion.presence,
                    px_scale,
                    detailed: !motion.still || edge > 0.0,
                };
                let (main, back) = &groups.lines[index];
                lyric_line(main, back, view, font, align)
            };
            let row = div()
                .id(arm_id.clone())
                .relative()
                .top(px(-offset))
                .w_full()
                .mb(px(font * 0.3))
                .px(px(14.))
                .py(px(font * 0.26))
                .rounded(px(14.))
                .opacity(edge)
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
                    if (m.view_h - height).abs() > 0.5 {
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
}
