use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

use crate::audio::equalizer::{EqDspContext, EqualizerSettings};
use crate::audio::filter::{FilterDspContext, FilterSettings};
use crate::audio::limiter::{LimiterDspContext, LimiterSettings};

pub const MAX_SLOTS: usize = 16;

pub trait DspNode: Send + Sync {
    fn configure(&self, sample_rate: u32, channels: u32);
    fn apply_settings(&self, settings: &EffectSettings, enabled: bool) -> bool;
    fn effect_settings(&self) -> EffectSettings;
    fn process_f32(&self, samples: &mut [f32]);
    fn process_i16(&self, samples: &mut [i16]);
    fn meter_db(&self) -> f64 {
        0.0
    }
    fn clear_meter(&self) {}
    fn process_count(&self) -> u64 {
        0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Equalizer,
    Filter,
    Limiter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "settings", rename_all = "snake_case")]
pub enum EffectSettings {
    Equalizer(EqualizerSettings),
    Filter(FilterSettings),
    Limiter(LimiterSettings),
}

impl EffectSettings {
    pub fn kind(&self) -> EffectKind {
        match self {
            Self::Equalizer(_) => EffectKind::Equalizer,
            Self::Filter(_) => EffectKind::Filter,
            Self::Limiter(_) => EffectKind::Limiter,
        }
    }

    pub fn clamp(self) -> Self {
        match self {
            Self::Equalizer(s) => Self::Equalizer(s.clamp()),
            Self::Filter(s) => Self::Filter(s.clamp()),
            Self::Limiter(s) => Self::Limiter(s.clamp()),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainSlotSettings {
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(flatten)]
    pub effect: EffectSettings,
}

pub struct ChainSlot {
    pub id: String,
    pub kind: EffectKind,
    pub bypassed: bool,
    pub node: Arc<dyn DspNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SlotStatus {
    pub id: String,
    pub kind: EffectKind,
    pub meter_db: f32,
    pub process_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DspChainStatus {
    pub attached: bool,
    pub process_count: u64,
    pub slots: Vec<SlotStatus>,
}

fn make_node(kind: EffectKind) -> Arc<dyn DspNode> {
    match kind {
        EffectKind::Equalizer => Arc::new(EqDspContext::new()),
        EffectKind::Filter => Arc::new(FilterDspContext::new()),
        EffectKind::Limiter => Arc::new(LimiterDspContext::new()),
    }
}

// ── Node adapters ────────────────────────────────────────────────────────────

impl DspNode for EqDspContext {
    fn configure(&self, sample_rate: u32, channels: u32) {
        self.configure_stream(sample_rate, channels);
    }

    fn apply_settings(&self, settings: &EffectSettings, enabled: bool) -> bool {
        let EffectSettings::Equalizer(s) = settings else {
            return false;
        };
        let mut s = s.clone();
        s.enabled = enabled && s.enabled;
        self.set_settings(s);
        true
    }

    fn effect_settings(&self) -> EffectSettings {
        EffectSettings::Equalizer(self.get_settings())
    }

    fn process_f32(&self, samples: &mut [f32]) {
        self.process_buffer_f32(samples);
    }

    fn process_i16(&self, samples: &mut [i16]) {
        self.process_buffer_i16(samples);
    }

    fn process_count(&self) -> u64 {
        EqDspContext::process_count(self)
    }
}

impl DspNode for FilterDspContext {
    fn configure(&self, sample_rate: u32, channels: u32) {
        self.configure_stream(sample_rate, channels);
    }

    fn apply_settings(&self, settings: &EffectSettings, enabled: bool) -> bool {
        let EffectSettings::Filter(s) = settings else {
            return false;
        };
        let mut s = s.clone();
        s.enabled = enabled && s.enabled;
        self.set_settings(s);
        true
    }

    fn effect_settings(&self) -> EffectSettings {
        EffectSettings::Filter(self.get_settings())
    }

    fn process_f32(&self, samples: &mut [f32]) {
        self.process_buffer_f32(samples);
    }

    fn process_i16(&self, samples: &mut [i16]) {
        self.process_buffer_i16(samples);
    }

    fn process_count(&self) -> u64 {
        FilterDspContext::process_count(self)
    }
}

impl DspNode for LimiterDspContext {
    fn configure(&self, sample_rate: u32, channels: u32) {
        self.configure_stream(sample_rate, channels);
    }

    fn apply_settings(&self, settings: &EffectSettings, enabled: bool) -> bool {
        let EffectSettings::Limiter(s) = settings else {
            return false;
        };
        let mut s = s.clone();
        s.enabled = enabled && s.enabled;
        self.set_settings(s);
        true
    }

    fn effect_settings(&self) -> EffectSettings {
        EffectSettings::Limiter(self.get_settings())
    }

    fn process_f32(&self, samples: &mut [f32]) {
        self.process_buffer_f32(samples);
    }

    fn process_i16(&self, samples: &mut [i16]) {
        self.process_buffer_i16(samples);
    }

    fn meter_db(&self) -> f64 {
        self.reduction_db()
    }

    fn clear_meter(&self) {
        LimiterDspContext::clear_meter(self);
    }

    fn process_count(&self) -> u64 {
        LimiterDspContext::process_count(self)
    }
}

pub struct DspChain {
    slots: ArcSwap<Vec<ChainSlot>>,
    sample_rate: AtomicU32,
    channels: AtomicU32,
    bytes_per_sample: AtomicU32,
    float_dsp: AtomicBool,
    dsp_float_forced: AtomicBool,
    attached: AtomicBool,
    process_count: AtomicU64,
}

impl Default for DspChain {
    fn default() -> Self {
        Self::new()
    }
}

impl DspChain {
    pub fn new() -> Self {
        Self {
            slots: ArcSwap::from_pointee(Vec::new()),
            sample_rate: AtomicU32::new(44100),
            channels: AtomicU32::new(2),
            bytes_per_sample: AtomicU32::new(4),
            float_dsp: AtomicBool::new(false),
            dsp_float_forced: AtomicBool::new(false),
            attached: AtomicBool::new(false),
            process_count: AtomicU64::new(0),
        }
    }

    pub fn set_float_dsp_enabled(&self, enabled: bool) {
        self.float_dsp.store(enabled, Ordering::Release);
    }

    pub fn set_dsp_float_forced(&self, forced: bool) {
        self.dsp_float_forced.store(forced, Ordering::Release);
        if forced {
            self.bytes_per_sample.store(4, Ordering::Release);
        }
    }

    pub fn set_attached(&self, attached: bool) {
        self.attached.store(attached, Ordering::Relaxed);
        if !attached {
            for slot in self.slots.load().iter() {
                slot.node.clear_meter();
            }
        }
    }

    pub fn is_attached(&self) -> bool {
        self.attached.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.slots.load().is_empty()
    }

    pub fn configure_stream(&self, sample_rate: u32, channels: u32, channel_flags: u32) {
        let chans = channels.max(1);
        let float_channel = channel_flags & crate::audio::bass::BASS_SAMPLE_FLOAT != 0;
        let bytes_per_sample = if self.dsp_float_forced.load(Ordering::Acquire)
            || self.float_dsp.load(Ordering::Acquire)
            || float_channel
        {
            4
        } else {
            2
        };

        let rate = if sample_rate > 0 { sample_rate } else { 44100 };
        self.sample_rate.store(rate, Ordering::Release);
        self.channels.store(chans, Ordering::Release);
        self.bytes_per_sample
            .store(bytes_per_sample, Ordering::Release);

        for slot in self.slots.load().iter() {
            slot.node.configure(rate, chans);
        }
    }

    pub fn apply(&self, desired: &[ChainSlotSettings]) {
        let rate = self.sample_rate.load(Ordering::Acquire);
        let chans = self.channels.load(Ordering::Acquire);
        let current = self.slots.load();

        let take = desired.len().min(MAX_SLOTS);
        let mut next: Vec<ChainSlot> = Vec::with_capacity(take);
        let mut claimed = vec![false; current.len()];

        for want in desired.iter().take(take) {
            let kind = want.effect.kind();
            let reused = current.iter().enumerate().find_map(|(i, slot)| {
                if !claimed[i] && slot.kind == kind && slot.id == want.id {
                    claimed[i] = true;
                    Some(Arc::clone(&slot.node))
                } else {
                    None
                }
            });
            let node = match reused {
                Some(node) => node,
                None => {
                    let node = make_node(kind);
                    node.configure(rate, chans);
                    node
                }
            };
            node.apply_settings(&want.effect, want.enabled);
            next.push(ChainSlot {
                id: want.id.clone(),
                kind,
                bypassed: !want.enabled,
                node,
            });
        }

        self.slots.store(Arc::new(next));
    }

    pub fn settings(&self) -> Vec<ChainSlotSettings> {
        self.slots
            .load()
            .iter()
            .map(|slot| ChainSlotSettings {
                id: slot.id.clone(),
                enabled: !slot.bypassed,
                effect: slot.node.effect_settings(),
            })
            .collect()
    }

    pub fn status(&self) -> DspChainStatus {
        DspChainStatus {
            attached: self.attached.load(Ordering::Relaxed),
            process_count: self.process_count.load(Ordering::Relaxed),
            slots: self
                .slots
                .load()
                .iter()
                .map(|slot| SlotStatus {
                    id: slot.id.clone(),
                    kind: slot.kind,
                    meter_db: slot.node.meter_db() as f32,
                    process_count: slot.node.process_count(),
                })
                .collect(),
        }
    }

    pub fn process_f32(&self, samples: &mut [f32]) {
        let slots = self.slots.load();
        if slots.is_empty() || samples.is_empty() {
            return;
        }
        self.process_count.fetch_add(1, Ordering::Relaxed);
        for slot in slots.iter() {
            if slot.bypassed {
                continue;
            }
            slot.node.process_f32(samples);
        }
    }

    pub fn process_i16(&self, samples: &mut [i16]) {
        let slots = self.slots.load();
        if slots.is_empty() || samples.is_empty() {
            return;
        }
        self.process_count.fetch_add(1, Ordering::Relaxed);
        for slot in slots.iter() {
            if slot.bypassed {
                continue;
            }
            slot.node.process_i16(samples);
        }
    }
}

pub unsafe extern "system" fn chain_dsp_callback(
    _handle: u32,
    _channel: u32,
    buffer: *mut std::ffi::c_void,
    length: u32,
    user: *mut std::ffi::c_void,
) {
    if buffer.is_null() || user.is_null() || length < 2 {
        return;
    }

    {
        static DSP_THREAD_REGISTERED: AtomicBool = AtomicBool::new(false);
        if !DSP_THREAD_REGISTERED.swap(true, Ordering::Relaxed) {
            crate::audio::process_util::register_audio_thread();
        }
    }

    let chain = &*(user as *const DspChain);
    let use_float = chain.dsp_float_forced.load(Ordering::Acquire)
        || chain.bytes_per_sample.load(Ordering::Acquire) >= 4;

    if use_float {
        let sample_count = (length / 4) as usize;
        if sample_count == 0 {
            return;
        }
        let samples = std::slice::from_raw_parts_mut(buffer as *mut f32, sample_count);
        chain.process_f32(samples);
    } else {
        let sample_count = (length / 2) as usize;
        if sample_count == 0 {
            return;
        }
        let samples = std::slice::from_raw_parts_mut(buffer as *mut i16, sample_count);
        chain.process_i16(samples);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn chain() -> DspChain {
        let c = DspChain::new();
        c.configure_stream(44100, 2, crate::audio::bass::BASS_SAMPLE_FLOAT);
        c
    }

    fn slot(id: &str, effect: EffectSettings) -> ChainSlotSettings {
        ChainSlotSettings {
            id: id.to_string(),
            enabled: true,
            effect,
        }
    }

    fn boost_eq() -> EffectSettings {
        EffectSettings::Equalizer(EqualizerSettings {
            enabled: true,
            preamp_db: 12.0,
            bands_db: [0.0; crate::audio::equalizer::BAND_COUNT],
        })
    }

    fn clip_limiter() -> EffectSettings {
        EffectSettings::Limiter(LimiterSettings {
            enabled: true,
            gain_db: 0.0,
            ceiling_db: 0.0,
            release_ms: 60.0,
            clip: true,
        })
    }

    fn sine(len: usize, amp: f32) -> Vec<f32> {
        (0..len)
            .map(|i| (2.0 * PI * 220.0 * i as f64 / 44100.0).sin() as f32 * amp)
            .collect()
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    #[test]
    fn reorder_preserves_node_identity() {
        let c = chain();
        c.apply(&[slot("a", boost_eq()), slot("b", clip_limiter())]);
        let (a_ptr, b_ptr) = {
            let slots = c.slots.load();
            (
                Arc::as_ptr(&slots[0].node) as *const u8,
                Arc::as_ptr(&slots[1].node) as *const u8,
            )
        };

        c.apply(&[slot("b", clip_limiter()), slot("a", boost_eq())]);
        let slots = c.slots.load();
        assert_eq!(slots[0].id, "b");
        assert_eq!(slots[1].id, "a");
        assert_eq!(
            Arc::as_ptr(&slots[0].node) as *const u8,
            b_ptr,
            "limiter node was rebuilt on reorder — delay ring would reset"
        );
        assert_eq!(
            Arc::as_ptr(&slots[1].node) as *const u8,
            a_ptr,
            "EQ node was rebuilt on reorder — IIR memory would reset"
        );
    }

    #[test]
    fn kind_change_replaces_node() {
        let c = chain();
        c.apply(&[slot("x", boost_eq())]);
        let before = Arc::as_ptr(&c.slots.load()[0].node) as *const u8;
        c.apply(&[slot("x", clip_limiter())]);
        let after = Arc::as_ptr(&c.slots.load()[0].node) as *const u8;
        assert_ne!(before, after, "node was reused across a kind change");
    }

    #[test]
    fn order_decides_whether_the_boost_escapes() {
        let c = chain();

        c.apply(&[slot("eq", boost_eq()), slot("lim", clip_limiter())]);
        let mut caught = sine(2048, 0.5);
        c.process_f32(&mut caught);
        assert!(
            peak(&caught) <= 1.0 + 1.0e-6,
            "limiter after EQ let the boost through: {:.4}",
            peak(&caught)
        );

        c.apply(&[slot("lim", clip_limiter()), slot("eq", boost_eq())]);
        let mut escaped = sine(2048, 0.5);
        c.process_f32(&mut escaped);
        assert!(
            peak(&escaped) > 1.5,
            "limiter before EQ should not be able to catch the boost: {:.4}",
            peak(&escaped)
        );
    }

    #[test]
    fn bypassed_slot_is_skipped() {
        let c = chain();
        c.apply(&[ChainSlotSettings {
            id: "eq".into(),
            enabled: false,
            effect: boost_eq(),
        }]);

        let dry = sine(1024, 0.25);
        let mut wet = dry.clone();
        c.process_f32(&mut wet);
        for (i, (&a, &b)) in dry.iter().zip(wet.iter()).enumerate() {
            assert!((a - b).abs() < 1.0e-9, "bypassed EQ altered sample {i}");
        }
        assert_eq!(
            c.status().slots[0].process_count,
            0,
            "bypassed node still processed a buffer"
        );
    }

    #[test]
    fn duplicate_kinds_stack_independently() {
        let c = chain();
        c.apply(&[slot("eq1", boost_eq()), slot("eq2", boost_eq())]);
        {
            let slots = c.slots.load();
            assert_ne!(
                Arc::as_ptr(&slots[0].node) as *const u8,
                Arc::as_ptr(&slots[1].node) as *const u8,
                "two slots aliased the same node"
            );
        }

        let mut buf = sine(1024, 0.1);
        c.process_f32(&mut buf);
        assert!(
            peak(&buf) > 1.2,
            "second EQ did not process: peak {:.4}",
            peak(&buf)
        );
    }

    #[test]
    fn slot_settings_round_trip() {
        let json = serde_json::to_string(&slot("s1", clip_limiter())).unwrap();
        assert!(
            json.contains("\"kind\":\"limiter\""),
            "unexpected shape: {json}"
        );
        let back: ChainSlotSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "s1");
        assert!(back.enabled);
        assert_eq!(back.effect.kind(), EffectKind::Limiter);
    }

    #[test]
    fn missing_enabled_defaults_on() {
        let back: ChainSlotSettings = serde_json::from_str(
            r#"{"id":"f1","kind":"filter","settings":{"enabled":true,"lp_hz":800,"hp_hz":20,"resonance":1.0}}"#,
        )
        .unwrap();
        assert!(back.enabled);
        assert_eq!(back.effect.kind(), EffectKind::Filter);
    }

    #[test]
    fn apply_truncates_past_max_slots() {
        let c = chain();
        let many: Vec<_> = (0..MAX_SLOTS + 8)
            .map(|i| slot(&format!("s{i}"), boost_eq()))
            .collect();
        c.apply(&many);
        assert_eq!(c.slots.load().len(), MAX_SLOTS);
    }
}
