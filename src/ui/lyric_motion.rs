pub const RICH_LEAD_MS: f64 = 150.0;
pub const LINE_LEAD_MS: f64 = 115.0;

const SWIPE_DURATION_RATIO: f32 = 1.6;
const SWIPE_LEAD_RATIO: f32 = 0.1;
const SWIPE_START_FROM: f32 = -0.2;
const SWIPE_START_TO: f32 = 1.4;
const SWIPE_END_FROM: f32 = -0.1;
const SWIPE_END_TO: f32 = 1.5;
pub const WORD_FADE: f32 = SWIPE_END_FROM - SWIPE_START_FROM;
/// Soft edge of the letter mask, as a fraction of the whole word. Braccato sets
/// `--mask-fade` so this matches the old 0.1-wide word gradient.
const LETTER_FADE_RATIO: f32 = 0.1;

const FADE_IN_MS: f32 = 330.0;
const FADE_OUT_MS: f32 = 500.0;
const PRESENCE_MS: f32 = 166.0;
pub const LINE_SETTLE_MS: f64 = 50.0;

/// Resting lines sit at this paint scale. The one being sung eases to 1.
pub const LINE_REST_SCALE: f32 = 0.95;
pub const LINE_SCALE_MS: f64 = 166.0;

const GLOW_DURATION_RATIO: f32 = 1.2;
const GLOW_MIN_MS: f32 = 1200.0;

const WOBBLE_MS: f32 = 1000.0;
const WOBBLE_PEAK: f32 = 0.125;
const WOBBLE_SETTLE: f32 = 0.75;
const WOBBLE_SHIFT_EM: f32 = 0.05;

const WAVE_MS: f32 = 900.0;
const WAVE_CREST: f32 = 0.4;
const WAVE_RISE_EM: f32 = 0.05;
const WAVE_SETTLE_EM: f32 = 0.02;

/// A syllable held longer than this blooms and each letter swells as it is sung.
pub const LONG_WORD_MS: f32 = 1500.0;
pub const EMPHASIS_SCALE: f32 = 1.11;

/// Seconds the scroll clock runs ahead of the sung word, so the line is in
/// place before the voice reaches it.
pub const SCROLL_LEAD_SEC: f64 = 0.5;
/// Upcoming lines inside this window share one scroll instead of each pulling
/// the view on their own.
pub const EARLY_SCROLL_SEC: f64 = 0.54;
const SCROLL_END_SEC: f64 = 0.5;
/// Where the sung line sits in the viewport. 0 is the top, 1 is the bottom.
/// The line being sung is centered.
pub const TARGET_RATIO: f32 = 0.5;

/// `cubic-bezier(0.86, 0, 0.2, 1)` — the line-scroll timing function.
pub const SCROLL_EASE: Bezier = Bezier(0.86, 0.0, 0.2, 1.0);

#[derive(Clone, Copy)]
pub struct Bezier(f32, f32, f32, f32);

pub const EASE: Bezier = Bezier(0.25, 0.1, 0.25, 1.0);
pub const EASE_IN_OUT: Bezier = Bezier(0.42, 0.0, 0.58, 1.0);
pub const EASE_OUT: Bezier = Bezier(0.0, 0.0, 0.58, 1.0);

impl Bezier {
    pub fn at(self, t: f32) -> f32 {
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        let Bezier(x1, y1, x2, y2) = self;
        let curve = |a: f32, b: f32, s: f32| {
            let u = 1.0 - s;
            3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
        };
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..20 {
            let mid = (lo + hi) * 0.5;
            if curve(x1, x2, mid) < t {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        curve(y1, y2, (lo + hi) * 0.5)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lit {
    pub edge: f32,
    pub fade: f32,
}

impl Lit {
    pub const FULL: Lit = Lit {
        edge: 1.0,
        fade: WORD_FADE,
    };

    pub fn at(self, x: f32) -> f32 {
        ((self.edge + self.fade - x) / self.fade).clamp(0.0, 1.0)
    }

    pub fn dark(self) -> bool {
        self.edge + self.fade <= 0.0
    }

    pub fn full(self) -> bool {
        self.edge >= 1.0
    }

    pub fn amount(self) -> f32 {
        (self.edge + self.fade * 0.5).clamp(0.0, 1.0)
    }
}

pub fn swipe_start(since_ms: f32, duration_ms: f32) -> f32 {
    let swipe = duration_ms * SWIPE_DURATION_RATIO;
    if swipe <= 0.0 {
        return SWIPE_START_TO;
    }
    let t = ((since_ms + duration_ms * SWIPE_LEAD_RATIO) / swipe).clamp(0.0, 1.0);
    SWIPE_START_FROM + (SWIPE_START_TO - SWIPE_START_FROM) * t
}

pub fn word_lit(start: f32) -> Lit {
    Lit {
        edge: start,
        fade: WORD_FADE,
    }
}

pub fn letter_lit(start: f32, index: usize, count: usize) -> Lit {
    let n = count.max(1) as f32;
    Lit {
        edge: start * n - index as f32,
        fade: LETTER_FADE_RATIO * n,
    }
}

pub fn line_synced_fade(since_ms: f32) -> f32 {
    EASE.at(since_ms / FADE_IN_MS)
}

/// How much of the highlight drop-shadow is left. Braccato starts at a full
/// `0.8rem` blur on the revealed glyphs and eases it to nothing. A box shadow
/// would light the whole line, so the player does not paint this.
#[allow(dead_code)]
pub fn glow(since_ms: f32, duration_ms: f32) -> f32 {
    let dur = (duration_ms * GLOW_DURATION_RATIO).max(GLOW_MIN_MS);
    if since_ms < 0.0 || since_ms >= dur {
        return 0.0;
    }
    1.0 - EASE.at(since_ms / dur)
}

pub fn wobble(since_ms: f32) -> f32 {
    if since_ms < 0.0 {
        return 0.0;
    }
    let p = EASE.at(since_ms / WOBBLE_MS);
    let k = if p < WOBBLE_PEAK {
        p / WOBBLE_PEAK
    } else if p < WOBBLE_SETTLE {
        1.0 - EASE_IN_OUT.at((p - WOBBLE_PEAK) / (WOBBLE_SETTLE - WOBBLE_PEAK))
    } else {
        0.0
    };
    WOBBLE_SHIFT_EM * k
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wave {
    pub delay_ms: f32,
    pub duration_ms: f32,
}

pub fn letter_waves(count: usize, duration_ms: f32) -> Vec<Wave> {
    let swipe = duration_ms * SWIPE_DURATION_RATIO;
    let lead = duration_ms * SWIPE_LEAD_RATIO;
    let rise = WAVE_MS * WAVE_CREST;
    let n = count.max(1) as f32;
    (0..count)
        .map(|index| {
            if swipe <= 0.0 {
                return Wave {
                    delay_ms: duration_ms * index as f32 / n,
                    duration_ms: WAVE_MS,
                };
            }
            let begin = (swipe * (index as f32 / n - SWIPE_END_FROM)
                / (SWIPE_END_TO - SWIPE_END_FROM))
                .clamp(0.0, swipe);
            let finish = (swipe * ((index + 1) as f32 / n - SWIPE_START_FROM)
                / (SWIPE_START_TO - SWIPE_START_FROM))
                .clamp(0.0, swipe);
            let window = (finish - begin).max(1.0);
            let touched = begin - lead;
            let lit = touched + window;
            if lit - rise > touched {
                Wave {
                    delay_ms: touched,
                    duration_ms: window / WAVE_CREST,
                }
            } else {
                Wave {
                    delay_ms: lit - rise,
                    duration_ms: WAVE_MS,
                }
            }
        })
        .collect()
}

fn wave_progress(since_ms: f32, wave: Wave) -> Option<f32> {
    let local = since_ms - wave.delay_ms;
    if local < 0.0 {
        return None;
    }
    Some((local / wave.duration_ms.max(1.0)).min(1.0))
}

pub fn float(since_ms: f32, wave: Wave) -> f32 {
    let Some(p) = wave_progress(since_ms, wave) else {
        return 0.0;
    };
    if p < WAVE_CREST {
        WAVE_RISE_EM * EASE_IN_OUT.at(p / WAVE_CREST)
    } else {
        let e = EASE_OUT.at((p - WAVE_CREST) / (1.0 - WAVE_CREST));
        WAVE_RISE_EM + (WAVE_SETTLE_EM - WAVE_RISE_EM) * e
    }
}

/// Scale of one letter in a held word. It reaches [`EMPHASIS_SCALE`] as the
/// swipe finishes lighting that letter, then settles back to 1.
pub fn emphasis(since_ms: f32, wave: Wave) -> f32 {
    let Some(p) = wave_progress(since_ms, wave) else {
        return 1.0;
    };
    let k = if p < WAVE_CREST {
        EASE_IN_OUT.at(p / WAVE_CREST)
    } else {
        1.0 - EASE_OUT.at((p - WAVE_CREST) / (1.0 - WAVE_CREST))
    };
    1.0 + (EMPHASIS_SCALE - 1.0) * k
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineMotion {
    pub highlight: f32,
    pub presence: f32,
    pub past: bool,
    pub still: bool,
}

/// Paint scale of one line. It grows to 1 as the line starts and settles back
/// after it ends. Layout stays at full size; only the paint uses this.
pub fn line_scale(now_ms: f64, start_ms: f64, end_ms: f64) -> f32 {
    if now_ms < start_ms {
        return LINE_REST_SCALE;
    }
    if now_ms < end_ms {
        let t = ((now_ms - start_ms) / LINE_SCALE_MS).clamp(0.0, 1.0) as f32;
        return LINE_REST_SCALE + (1.0 - LINE_REST_SCALE) * EASE.at(t);
    }
    let t = ((now_ms - end_ms) / LINE_SCALE_MS).clamp(0.0, 1.0) as f32;
    1.0 + (LINE_REST_SCALE - 1.0) * EASE.at(t)
}

pub fn line_motion(start_ms: f64, end_ms: f64, now_ms: f64) -> LineMotion {
    if now_ms < start_ms {
        return LineMotion {
            highlight: 1.0,
            presence: 0.0,
            past: false,
            still: true,
        };
    }
    if now_ms < end_ms {
        let p = EASE.at(((now_ms - start_ms) as f32) / PRESENCE_MS);
        return LineMotion {
            highlight: 1.0,
            presence: p,
            past: false,
            still: false,
        };
    }
    let out = (now_ms - end_ms) as f32;
    let fade = 1.0 - EASE.at(out / FADE_OUT_MS);
    LineMotion {
        highlight: fade,
        presence: fade,
        past: true,
        still: out >= FADE_OUT_MS,
    }
}

#[derive(Clone, Copy)]
pub struct LyricBox {
    pub time: f64,
    pub duration: f64,
    pub top: f32,
    pub height: f32,
}

pub struct PlannedScroll {
    pub target: f32,
    pub active: Vec<usize>,
    pub focus: usize,
    pub pin: Option<usize>,
    pub commit: bool,
    /// A step the song itself took. A seek or a return from manual scrolling
    /// keeps every line on nearly the same duration so the block does not tear.
    pub natural: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScrollSide {
    Above,
    Active,
    Below,
}

fn scroll_side(relative: i32, delta: f32) -> ScrollSide {
    let upward = delta < 0.0;
    if relative < 0 {
        if upward { ScrollSide::Below } else { ScrollSide::Above }
    } else if relative > 0 {
        if upward { ScrollSide::Above } else { ScrollSide::Below }
    } else {
        ScrollSide::Active
    }
}

fn scroll_duration_ms(side: ScrollSide, distance: usize) -> f32 {
    let n = distance as f32;
    match side {
        ScrollSide::Above => 750.0 + n.min(6.0) * 20.0,
        ScrollSide::Active | ScrollSide::Below => {
            750.0 + (n + 2.0).ln() * 80.0 + (n + 1.0) * 20.0
        }
    }
}

/// How long this line takes to absorb one scroll, in milliseconds.
/// `delta` is the new scroll minus the one on screen. `step` is the gap to the
/// neighbouring line, used to stop a seek from spreading the lines apart.
pub fn line_shift_ms(relative: i32, delta: f32, distance: f32, step: f32, natural: bool) -> f32 {
    let own = scroll_duration_ms(scroll_side(relative, delta), relative.unsigned_abs() as usize);
    let active = scroll_duration_ms(scroll_side(0, delta), 0);
    let scale = if natural || distance <= 1.0 {
        1.0
    } else {
        (step / distance).min(1.0)
    };
    active + (own - active) * scale
}

pub fn neighbor_step(lines: &[LyricBox], focus: usize) -> f32 {
    let line = &lines[focus];
    let mut step = line.height.max(1.0);
    if focus > 0 {
        step = step.min((line.top - lines[focus - 1].top).abs());
    }
    if focus + 1 < lines.len() {
        step = step.min((lines[focus + 1].top - line.top).abs());
    }
    step.max(1.0)
}

/// A line the scroll will carry through either the old viewport or the new one.
pub fn line_crosses_scroll(top: f32, height: f32, from: f32, to: f32, view_h: f32) -> bool {
    let low = from.min(to);
    let high = from.max(to) + view_h;
    top + height >= low && top <= high
}

fn active_scroll_lines(lines: &[LyricBox], scroll_time: f64) -> Vec<usize> {
    let mut active = Vec::new();
    for index in 0..lines.len() {
        let time = lines[index].time;
        let next = lines
            .get(index + 1)
            .map(|line| line.time)
            .unwrap_or(f64::INFINITY);
        let duration = lines[index].duration;
        if scroll_time >= time - EARLY_SCROLL_SEC
            && (scroll_time < next || scroll_time < time + duration)
        {
            active.push(index);
        }
    }
    if active.is_empty() {
        active.push(0);
    }
    active
}

fn sung_lines(lines: &[LyricBox], active: &[usize], scroll_time: f64) -> Vec<usize> {
    let last = active.len() - 1;
    active
        .iter()
        .enumerate()
        .filter(|(place, index)| {
            *place == last
                || scroll_time < lines[**index].time + lines[**index].duration - SCROLL_END_SEC
        })
        .map(|(_, index)| *index)
        .collect()
}

fn anchor_of(lines: &[LyricBox], sung: &[usize], scroll_time: f64) -> usize {
    sung.iter()
        .rev()
        .find(|&&index| {
            let line = &lines[index];
            line.time <= scroll_time && scroll_time < line.time + line.duration
        })
        .or_else(|| {
            sung.iter()
                .rev()
                .find(|&&index| lines[index].time <= scroll_time)
        })
        .copied()
        .unwrap_or(sung[0])
}

fn scroll_target(lines: &[LyricBox], sung: &[usize], anchor: usize, view_h: f32) -> f32 {
    let offset = view_h * TARGET_RATIO;
    let average = sung
        .iter()
        .map(|&index| lines[index].top + lines[index].height * 0.5)
        .sum::<f32>()
        / sung.len() as f32;
    let first = &lines[sung[0]];
    let last = &lines[*sung.last().unwrap()];
    let mark = &lines[anchor];
    let mut scroll = average - offset;
    scroll = scroll.max(last.top + last.height - view_h);
    scroll = scroll.min(last.top);
    scroll = scroll.min(first.top);
    scroll = scroll.max(mark.top + mark.height - view_h);
    scroll = scroll.min(mark.top);
    scroll
}

/// The next scroll position. `previous` is the group already committed, including
/// lines that were only inside the lookahead. `force` is a seek or a return from
/// a manual scroll: both retarget immediately.
pub fn plan_scroll(
    lines: &[LyricBox],
    scroll_time: f64,
    view_h: f32,
    previous: &[usize],
    pinned: Option<usize>,
    force: bool,
) -> Option<PlannedScroll> {
    if lines.is_empty() || view_h <= 0.0 {
        return None;
    }
    let active = active_scroll_lines(lines, scroll_time);
    let sung = sung_lines(lines, &active, scroll_time);
    let anchor = anchor_of(lines, &sung, scroll_time);
    let first = &lines[sung[0]];
    let last = &lines[*sung.last().unwrap()];
    let overflows = last.top + last.height - first.top > view_h;
    let pin = overflows.then_some(anchor);
    let arrived = active.iter().any(|&index| {
        scroll_time >= lines[index].time && !previous.contains(&index)
    });
    let pin_done = pinned.is_some_and(|index| {
        index >= lines.len()
            || !sung.contains(&index)
            || scroll_time >= lines[index].time + lines[index].duration
    });
    Some(PlannedScroll {
        target: scroll_target(lines, &sung, anchor, view_h),
        focus: *active.last().unwrap(),
        active,
        pin,
        commit: force || arrived || pin_done || previous.is_empty(),
        natural: !force,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_matches_css_endpoints_and_midpoint() {
        assert_eq!(EASE.at(0.0), 0.0);
        assert_eq!(EASE.at(1.0), 1.0);
        assert!((EASE.at(0.5) - 0.8024).abs() < 0.002);
        assert!((EASE_IN_OUT.at(0.5) - 0.5).abs() < 0.002);
    }

    #[test]
    fn swipe_covers_the_word_after_its_duration() {
        let lit = word_lit(swipe_start(-100.0, 1000.0));
        assert!(lit.dark());
        assert!(word_lit(swipe_start(0.0, 1000.0)).at(0.0) < 0.5);
        assert!(word_lit(swipe_start(1100.0, 1000.0)).full());
    }

    #[test]
    fn letters_light_in_order() {
        let start = swipe_start(500.0, 1000.0);
        let first = letter_lit(start, 0, 4);
        let last = letter_lit(start, 3, 4);
        assert!(first.amount() > last.amount());
        assert!((first.fade - 0.4).abs() < 1e-4);
        assert!(letter_lit(swipe_start(2000.0, 1000.0), 3, 4).full());
    }

    #[test]
    fn waves_follow_the_swipe() {
        let waves = letter_waves(5, 2000.0);
        assert!(waves
            .windows(2)
            .all(|pair| pair[0].delay_ms < pair[1].delay_ms));
        assert_eq!(float(waves[0].delay_ms - 1.0, waves[0]), 0.0);
        assert!((float(10_000.0, waves[0]) - WAVE_SETTLE_EM).abs() < 1e-4);
        let crest = waves[0].delay_ms + waves[0].duration_ms * WAVE_CREST;
        assert!((emphasis(crest, waves[0]) - EMPHASIS_SCALE).abs() < 0.02);
        assert_eq!(emphasis(waves[0].delay_ms - 1.0, waves[0]), 1.0);
        assert!((emphasis(waves[0].delay_ms + waves[0].duration_ms, waves[0]) - 1.0).abs() < 0.02);
    }

    #[test]
    fn glow_and_wobble_end() {
        assert_eq!(glow(-1.0, 500.0), 0.0);
        assert!((glow(0.0, 500.0) - 1.0).abs() < 1e-4);
        assert!(glow(600.0, 500.0) < 0.5);
        assert!(glow(600.0, 500.0) > 0.05);
        assert_eq!(glow(1300.0, 500.0), 0.0);
        assert_eq!(wobble(-1.0), 0.0);
        assert!(wobble(60.0) > 0.0);
        assert_eq!(wobble(1000.0), 0.0);
    }

    #[test]
    fn the_sung_line_grows_and_the_finished_one_settles() {
        assert_eq!(line_scale(900.0, 1000.0, 3000.0), LINE_REST_SCALE);
        assert!((line_scale(1000.0, 1000.0, 3000.0) - LINE_REST_SCALE).abs() < 1e-4);
        assert!((line_scale(1000.0 + LINE_SCALE_MS, 1000.0, 3000.0) - 1.0).abs() < 1e-4);
        assert!((line_scale(2000.0, 1000.0, 3000.0) - 1.0).abs() < 1e-4);
        assert!((line_scale(3000.0 + LINE_SCALE_MS, 1000.0, 3000.0) - LINE_REST_SCALE).abs() < 1e-4);
        let mid = line_scale(1000.0 + LINE_SCALE_MS * 0.5, 1000.0, 3000.0);
        assert!(mid > LINE_REST_SCALE && mid < 1.0);
    }

    #[test]
    fn line_brightens_and_fades_out() {
        let before = line_motion(1000.0, 3000.0, 900.0);
        assert!(before.still && before.presence == 0.0);
        let during = line_motion(1000.0, 3000.0, 2000.0);
        assert_eq!(during.presence, 1.0);
        assert!(!during.still);
        let after = line_motion(1000.0, 3000.0, 3250.0);
        assert!(after.past && after.highlight > 0.0 && after.highlight < 1.0);
        assert!(line_motion(1000.0, 3000.0, 3600.0).still);
    }

    fn box_at(time: f64, top: f32) -> LyricBox {
        LyricBox {
            time,
            duration: 2.0,
            top,
            height: 48.0,
        }
    }

    #[test]
    fn scroll_easing_rests_at_both_ends() {
        assert_eq!(SCROLL_EASE.at(0.0), 0.0);
        assert_eq!(SCROLL_EASE.at(1.0), 1.0);
        assert!(SCROLL_EASE.at(0.2) < 0.2);
        assert!(SCROLL_EASE.at(0.8) > 0.8);
    }

    #[test]
    fn close_lines_share_one_scroll() {
        let lines = vec![box_at(0.0, 0.0), box_at(0.4, 80.0), box_at(4.0, 160.0)];
        let first = plan_scroll(&lines, 0.0, 600.0, &[], None, false).unwrap();
        assert!(first.commit);
        assert_eq!(first.active, vec![0, 1]);
        let held = plan_scroll(&lines, 0.4, 600.0, &first.active, first.pin, false).unwrap();
        assert!(!held.commit);
        let next = plan_scroll(&lines, 4.0, 600.0, &first.active, first.pin, false).unwrap();
        assert!(next.commit);
        assert!(next.active.contains(&2));
    }

    #[test]
    fn the_sung_line_sits_in_the_center() {
        let lines = vec![box_at(5.0, 500.0)];
        let plan = plan_scroll(&lines, 5.0, 1000.0, &[0], None, true).unwrap();
        let center = 500.0 + 24.0;
        assert!((plan.target - (center - 500.0)).abs() < 0.5);
    }

    #[test]
    fn lines_ahead_of_the_scroll_settle_before_lines_behind() {
        let ahead = line_shift_ms(-1, 80.0, 80.0, 48.0, true);
        let behind = line_shift_ms(1, 80.0, 80.0, 48.0, true);
        let active = line_shift_ms(0, 80.0, 80.0, 48.0, true);
        assert!((ahead - 770.0).abs() < 0.1);
        assert!((active - (750.0 + 2.0_f32.ln() * 80.0 + 20.0)).abs() < 0.1);
        assert!(ahead < behind);
        let seek_ahead = line_shift_ms(-1, 1000.0, 1000.0, 48.0, false);
        let seek_behind = line_shift_ms(1, 1000.0, 1000.0, 48.0, false);
        assert!(seek_behind - seek_ahead < behind - ahead);
    }
}
