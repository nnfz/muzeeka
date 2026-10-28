pub const RICH_LEAD_MS: f64 = 150.0;
pub const LINE_LEAD_MS: f64 = 115.0;

const SWIPE_DURATION_RATIO: f32 = 1.6;
const SWIPE_LEAD_RATIO: f32 = 0.1;
const SWIPE_START_FROM: f32 = -0.2;
const SWIPE_START_TO: f32 = 1.4;
const SWIPE_END_FROM: f32 = -0.1;
const SWIPE_END_TO: f32 = 1.5;
pub const WORD_FADE: f32 = SWIPE_END_FROM - SWIPE_START_FROM;
const LETTER_MASK_FADE: f32 = 0.03;

const FADE_IN_MS: f32 = 330.0;
const FADE_OUT_MS: f32 = 500.0;
const PRESENCE_MS: f32 = 166.0;
pub const LINE_SETTLE_MS: f64 = 50.0;

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
        fade: LETTER_MASK_FADE * (n + 2.0),
    }
}

pub fn line_synced_fade(since_ms: f32) -> f32 {
    EASE.at(since_ms / FADE_IN_MS)
}

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

pub fn float(since_ms: f32, wave: Wave) -> f32 {
    let local = since_ms - wave.delay_ms;
    if local < 0.0 {
        return 0.0;
    }
    let p = (local / wave.duration_ms.max(1.0)).min(1.0);
    if p < WAVE_CREST {
        WAVE_RISE_EM * EASE_IN_OUT.at(p / WAVE_CREST)
    } else {
        let e = EASE_OUT.at((p - WAVE_CREST) / (1.0 - WAVE_CREST));
        WAVE_RISE_EM + (WAVE_SETTLE_EM - WAVE_RISE_EM) * e
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineMotion {
    pub highlight: f32,
    pub presence: f32,
    pub past: bool,
    pub still: bool,
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
        let first = letter_lit(start, 0, 4).amount();
        let last = letter_lit(start, 3, 4).amount();
        assert!(first > last);
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
    }

    #[test]
    fn glow_and_wobble_end() {
        assert_eq!(glow(-1.0, 500.0), 0.0);
        assert!(glow(0.0, 500.0) > 0.99);
        assert_eq!(glow(1300.0, 500.0), 0.0);
        assert_eq!(wobble(-1.0), 0.0);
        assert!(wobble(60.0) > 0.0);
        assert_eq!(wobble(1000.0), 0.0);
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
}
