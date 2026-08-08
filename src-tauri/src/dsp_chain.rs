//! Modular DSP rack for the mixer output — a foobar2000-style effect chain.
//!
//! One BASS DSP callback sits on the mixer and walks an ordered list of effect
//! nodes internally. That is deliberate: BASS runs DSPs highest-priority-first,
//! so "N effects in an arbitrary order, any of them more than once" cannot be
//! expressed as priorities — every reorder would mean detach/reattach, which is
//! audible. Here order is just the index in a `Vec`, and reorder / add / remove
//! are a single `ArcSwap::store` with no BASS call and no gap in the audio.
//!
//! Realtime notes:
//! - The slot list lives in `ArcSwap`, so the audio callback never waits on UI
//!   writes and a removed node is only dropped once the callback releases its
//!   guard. That is what retires the per-effect `Box::leak`.
//! - The float/int16 decision happens once per buffer here instead of once per
//!   effect, so the nodes themselves no longer carry format state.
//! - `DspChain` itself is `Box::leak`ed exactly once at startup, because BASS
//!   holds a raw `user` pointer to it for the process lifetime.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

use crate::equalizer::{EqDspContext, EqualizerSettings};
use crate::filter::{FilterDspContext, FilterSettings};
use crate::limiter::{LimiterDspContext, LimiterSettings};

/// Upper bound on chain length. Each slot is a full biquad cascade or a
/// look-ahead limiter on every buffer, so an unbounded rack is a way to make the
/// audio thread miss its deadline. 16 is far more than any real chain.
pub const MAX_SLOTS: usize = 16;

/// One effect's worth of live processing.
///
/// `enabled` is intentionally *not* part of `configure`/`apply_settings`'s own
/// payload — the slot owns it, so a single checkbox in the rack is the only
/// switch and the node's internal flag can never disagree with the UI.
pub trait DspNode: Send + Sync {
    /// Re-derive rate/channel-dependent state. Called on attach and whenever the
    /// mixer format changes.
    fn configure(&self, sample_rate: u32, channels: u32);
    /// Push new parameters. Returns false if the payload is for another kind.
    fn apply_settings(&self, settings: &EffectSettings, enabled: bool) -> bool;
    /// Current parameters, for `player_get_dsp_chain`.
    fn effect_settings(&self) -> EffectSettings;
    fn process_f32(&self, samples: &mut [f32]);
    fn process_i16(&self, samples: &mut [i16]);
    /// Live readout for the UI (limiter gain reduction). 0 = nothing to show.
    fn meter_db(&self) -> f64 {
        0.0
    }
    /// Zero the meter. Called on detach: no callback will run to clear it, so a
    /// stale reading would look like the effect is still working.
    fn clear_meter(&self) {}
    /// Buffers this node has actually processed — separates "in the chain" from
    /// "seeing audio".
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

/// Adjacently tagged so the effect's own `enabled` stays nested under `settings`
/// and cannot collide with the slot's `enabled` when the slot is flattened.
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

/// One rack row as it is persisted and sent over IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainSlotSettings {
    /// Stable id minted by the frontend. Identity, not order — it is what lets a
    /// reordered slot keep its filter memory.
    pub id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(flatten)]
    pub effect: EffectSettings,
}

/// Live rack row. Held inside an immutable `Arc<Vec<_>>`, so `bypassed` is a
/// plain `bool` — `apply` rebuilds the list rather than mutating a slot in place.
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
    /// Meaning is per-kind; today only the limiter reports anything.
    pub meter_db: f32,
    pub process_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DspChainStatus {
    /// Whether the chain DSP is on the mixer at all.
    pub attached: bool,
    /// Buffers the chain has processed — distinguishes attached from running.
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
//
// Each effect keeps its own settings type and process functions; the trait only
// wires them into the rack. `apply_settings` folds the slot's `enabled` into the
// effect's own flag, so bypassing a row and switching the effect off are the same
// thing and cannot drift apart.

impl DspNode for EqDspContext {
    fn configure(&self, sample_rate: u32, channels: u32) {
        // The chain has already forced float on the buffer, so the flags argument
        // only matters for the fallback path — which the chain handles itself.
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
    /// Bytes per sample in the DSP buffer (2 = int16, 4 = float32).
    bytes_per_sample: AtomicU32,
    /// Set when BASS_CONFIG_FLOATDSP is active — buffer is always float32.
    float_dsp: AtomicBool,
    /// Set when the DSP was attached with BASS_DSP_FLOAT.
    dsp_float_forced: AtomicBool,
    /// Mirrors `PlayerInner::chain_dsp_handle != 0`, so the UI can poll status
    /// without taking the player lock.
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
        let float_channel = channel_flags & crate::bass::BASS_SAMPLE_FLOAT != 0;
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

    /// Rebuild the chain from the desired list, reusing nodes by identity.
    ///
    /// A slot whose `id` **and** kind already exist keeps its existing node, so
    /// its IIR memory and the limiter's delay ring travel with it through a
    /// reorder — that is what makes dragging a row glitch-free, and it also means
    /// a slider drag allocates nothing but the slot list itself.
    pub fn apply(&self, desired: &[ChainSlotSettings]) {
        let rate = self.sample_rate.load(Ordering::Acquire);
        let chans = self.channels.load(Ordering::Acquire);
        let current = self.slots.load();

        let take = desired.len().min(MAX_SLOTS);
        let mut next: Vec<ChainSlot> = Vec::with_capacity(take);
        // Each existing node may be claimed once. Two slots sharing one node
        // would share its filter memory, so a duplicated id must not alias.
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

    /// Walk the chain in order over interleaved 32-bit float PCM.
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

    /// Walk the chain in order over interleaved 16-bit PCM.
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

/// BASS DSP callback — must match DSPPROC signature. `user` = `*const DspChain`.
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

    // First call runs on BASS's mixer/update thread — register it as audio-critical
    // so process BELOW_NORMAL (unfocused window) does not starve the callback.
    {
        static DSP_THREAD_REGISTERED: AtomicBool = AtomicBool::new(false);
        if !DSP_THREAD_REGISTERED.swap(true, Ordering::Relaxed) {
            crate::process_util::register_audio_thread();
        }
    }

    let chain = &*(user as *const DspChain);
    // One format decision for the whole rack, instead of one per effect.
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
        c.configure_stream(44100, 2, crate::bass::BASS_SAMPLE_FLOAT);
        c
    }

    fn slot(id: &str, effect: EffectSettings) -> ChainSlotSettings {
        ChainSlotSettings {
            id: id.to_string(),
            enabled: true,
            effect,
        }
    }

    /// A +12 dB preamp with flat bands is pure gain, so order is easy to reason about.
    fn boost_eq() -> EffectSettings {
        EffectSettings::Equalizer(EqualizerSettings {
            enabled: true,
            preamp_db: 12.0,
            bands_db: [0.0; crate::equalizer::BAND_COUNT],
        })
    }

    /// Clip mode, so the ceiling applies to the very first sample with no
    /// look-ahead delay to prime.
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

    /// The point of reuse-by-id: a reorder must carry the same node object, or
    /// every drag would reset filter memory and click.
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

    /// Same id, different kind, must not hand a limiter's node to an EQ.
    #[test]
    fn kind_change_replaces_node() {
        let c = chain();
        c.apply(&[slot("x", boost_eq())]);
        let before = Arc::as_ptr(&c.slots.load()[0].node) as *const u8;
        c.apply(&[slot("x", clip_limiter())]);
        let after = Arc::as_ptr(&c.slots.load()[0].node) as *const u8;
        assert_ne!(before, after, "node was reused across a kind change");
    }

    /// The honest proof that order is real: with the limiter last it catches the
    /// EQ's boost; with the limiter first the boost escapes past the ceiling.
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

    /// Bypass has to be free, not just quiet — the node must not run at all.
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

    /// Two of the same effect must be two independent instances, stacking.
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
        // +12 dB twice ≈ ×15.85 on a 0.1 peak sine.
        assert!(
            peak(&buf) > 1.2,
            "second EQ did not process: peak {:.4}",
            peak(&buf)
        );
    }

    /// Round-trips through JSON, since this is what lands in settings.json.
    #[test]
    fn slot_settings_round_trip() {
        let json = serde_json::to_string(&slot("s1", clip_limiter())).unwrap();
        assert!(json.contains("\"kind\":\"limiter\""), "unexpected shape: {json}");
        let back: ChainSlotSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "s1");
        assert!(back.enabled);
        assert_eq!(back.effect.kind(), EffectKind::Limiter);
    }

    /// A hand-written chain missing `enabled` should default to on, not silently off.
    #[test]
    fn missing_enabled_defaults_on() {
        let back: ChainSlotSettings = serde_json::from_str(
            r#"{"id":"f1","kind":"filter","settings":{"enabled":true,"lp_hz":800,"hp_hz":20,"resonance":1.0}}"#,
        )
        .unwrap();
        assert!(back.enabled);
        assert_eq!(back.effect.kind(), EffectKind::Filter);
    }

    /// Chain length is bounded, so a malformed settings file cannot pin the audio thread.
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
