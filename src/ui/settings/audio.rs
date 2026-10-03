//! Audio settings, matching the web rack: chain, equalizer, filter, limiter,
//! presets, playback speed, and the pitch switch. Sliders apply live. The file
//! is patched when the pointer goes up, and only the audio keys are replaced.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{
    canvas, div, prelude::*, px, relative, AnyElement, Bounds, Context, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, SharedString, Styled, Window,
};

use muzeeka::audio::dsp_chain::{ChainSlotSettings, EffectKind, EffectSettings, MAX_SLOTS};
use muzeeka::audio::equalizer::{EqualizerSettings, BAND_COUNT, BAND_FREQUENCIES};
use muzeeka::audio::filter::{FilterSettings, HP_OPEN_HZ, LP_OPEN_HZ};
use muzeeka::audio::limiter::LimiterSettings;

use crate::ui::icons::{self, icon};
use crate::ui::playback::{AudioRack, ChainPreset, EqPreset, FilterPreset, LimiterPreset};
use crate::ui::session::{Session, SettingsSection};
use crate::ui::theme;

use super::{card, section_heading, SettingsWindow};

const RATE_PRESETS: [f32; 5] = [0.75, 0.85, 1.0, 1.25, 1.5];

pub(super) struct AudioUi {
    expanded: HashSet<String>,
    drag: Option<Drag>,
    bounds: HashMap<String, Bounds<Pixels>>,
    menu: Menu,
    meters: HashMap<String, f32>,
    drop_at: Option<usize>,
}

#[derive(Clone)]
enum Drag {
    Knob {
        id: String,
        knob: Knob,
        vertical: bool,
    },
    Reorder {
        id: String,
        origin_y: f32,
        active: bool,
    },
}

#[derive(Clone)]
enum Knob {
    Rate,
    Preamp { slot: String },
    Band { slot: String, index: usize },
    Highpass { slot: String },
    Lowpass { slot: String },
    Resonance { slot: String },
    Gain { slot: String },
    Ceiling { slot: String },
    Release { slot: String },
}

enum Menu {
    Closed,
    Chain { saving: bool },
    Effect { slot: String, saving: bool },
}

impl AudioUi {
    pub(super) fn new() -> Self {
        Self {
            expanded: HashSet::new(),
            drag: None,
            bounds: HashMap::new(),
            menu: Menu::Closed,
            meters: HashMap::new(),
            drop_at: None,
        }
    }
}

impl Session {
    pub(crate) fn preview_rate(&mut self, rate: f32) {
        self.rack.playback_rate = if rate.is_finite() {
            rate.clamp(0.25, 2.0)
        } else {
            1.0
        };
    }

    pub(crate) fn commit_rate(&mut self) {
        let rate = self.rack.playback_rate;
        if let Some(engine) = &self.engine {
            if let Err(error) = engine.set_rate(rate) {
                self.push_notice(format!("Playback rate was not applied: {error}"));
            }
        }
        self.rack.save();
    }

    pub(crate) fn set_pitch_coupled(&mut self, enabled: bool) {
        self.rack.pitch_enabled = enabled;
        if let Some(engine) = &self.engine {
            if let Err(error) = engine.set_pitch(enabled) {
                self.push_notice(format!("Pitch mode was not applied: {error}"));
            }
        }
        self.rack.save();
    }

    pub(crate) fn commit_chain(&mut self) {
        self.rack.save();
    }

    pub(crate) fn add_effect(&mut self, kind: EffectKind) -> Option<String> {
        if self.rack.chain.len() >= MAX_SLOTS {
            return None;
        }
        let id = mint_id(kind);
        self.rack.chain.push(ChainSlotSettings {
            id: id.clone(),
            enabled: true,
            effect: fresh_effect(kind),
        });
        self.push_chain(true);
        Some(id)
    }

    pub(crate) fn remove_effect(&mut self, id: &str) {
        let before = self.rack.chain.len();
        self.rack.chain.retain(|slot| slot.id != id);
        if self.rack.chain.len() != before {
            self.push_chain(true);
        }
    }

    pub(crate) fn clear_chain(&mut self) {
        if self.rack.chain.is_empty() {
            return;
        }
        self.rack.chain.clear();
        self.push_chain(true);
    }

    pub(crate) fn nudge_effect(&mut self, id: &str, dir: i32) {
        let Some(from) = self.rack.chain.iter().position(|slot| slot.id == id) else {
            return;
        };
        let to = from as i32 + dir;
        if to < 0 || to >= self.rack.chain.len() as i32 {
            return;
        }
        self.rack.chain.swap(from, to as usize);
        self.push_chain(true);
    }

    /// `to` is the gap index, including one past the last row.
    pub(crate) fn move_effect(&mut self, id: &str, to: usize) {
        let Some(from) = self.rack.chain.iter().position(|slot| slot.id == id) else {
            return;
        };
        let mut to = to.min(self.rack.chain.len());
        if to > from {
            to -= 1;
        }
        if to == from {
            return;
        }
        let slot = self.rack.chain.remove(from);
        self.rack.chain.insert(to, slot);
        self.push_chain(true);
    }

    pub(crate) fn set_slot_power(&mut self, id: &str, enabled: bool) {
        self.edit_slot(id, true, |slot| slot.enabled = enabled);
    }

    pub(crate) fn reset_slot(&mut self, id: &str) {
        let Some(kind) = self
            .rack
            .chain
            .iter()
            .find(|slot| slot.id == id)
            .map(|slot| slot.effect.kind())
        else {
            return;
        };
        self.edit_slot(id, true, |slot| {
            slot.enabled = true;
            slot.effect = fresh_effect(kind);
        });
    }

    pub(crate) fn set_eq_preamp(&mut self, id: &str, db: f32) {
        self.edit_slot(id, false, |slot| {
            if let EffectSettings::Equalizer(eq) = &mut slot.effect {
                eq.preamp_db = db;
            }
        });
    }

    pub(crate) fn set_eq_band(&mut self, id: &str, index: usize, db: f32) {
        self.edit_slot(id, false, |slot| {
            if let EffectSettings::Equalizer(eq) = &mut slot.effect {
                if let Some(band) = eq.bands_db.get_mut(index) {
                    *band = db;
                }
            }
        });
    }

    pub(crate) fn set_filter_hp(&mut self, id: &str, hz: f32) {
        self.edit_slot(id, false, |slot| {
            if let EffectSettings::Filter(filter) = &mut slot.effect {
                filter.hp_hz = hz;
            }
        });
    }

    pub(crate) fn set_filter_lp(&mut self, id: &str, hz: f32) {
        self.edit_slot(id, false, |slot| {
            if let EffectSettings::Filter(filter) = &mut slot.effect {
                filter.lp_hz = hz;
            }
        });
    }

    pub(crate) fn set_filter_q(&mut self, id: &str, q: f32) {
        self.edit_slot(id, false, |slot| {
            if let EffectSettings::Filter(filter) = &mut slot.effect {
                filter.resonance = q;
            }
        });
    }

    pub(crate) fn set_limiter_gain(&mut self, id: &str, db: f32) {
        self.edit_slot(id, false, |slot| {
            if let EffectSettings::Limiter(limiter) = &mut slot.effect {
                limiter.gain_db = db;
            }
        });
    }

    pub(crate) fn set_limiter_ceiling(&mut self, id: &str, db: f32) {
        self.edit_slot(id, false, |slot| {
            if let EffectSettings::Limiter(limiter) = &mut slot.effect {
                limiter.ceiling_db = db;
            }
        });
    }

    pub(crate) fn set_limiter_release(&mut self, id: &str, ms: f32) {
        self.edit_slot(id, false, |slot| {
            if let EffectSettings::Limiter(limiter) = &mut slot.effect {
                limiter.release_ms = ms;
            }
        });
    }

    pub(crate) fn set_limiter_clip(&mut self, id: &str, clip: bool) {
        self.edit_slot(id, true, |slot| {
            if let EffectSettings::Limiter(limiter) = &mut slot.effect {
                limiter.clip = clip;
            }
        });
    }

    pub(crate) fn apply_chain_preset(&mut self, name: &str) {
        let Some(preset) = self
            .rack
            .chain_presets
            .iter()
            .find(|preset| preset.name == name)
            .cloned()
        else {
            return;
        };
        self.rack.chain = preset
            .slots
            .into_iter()
            .take(MAX_SLOTS)
            .map(|mut slot| {
                slot.id = mint_id(slot.effect.kind());
                slot.effect = slot.effect.clone().clamp();
                slot
            })
            .collect();
        self.push_chain(true);
    }

    pub(crate) fn save_chain_preset(&mut self, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        self.rack.chain_presets.retain(|preset| preset.name != name);
        self.rack.chain_presets.push(ChainPreset {
            name,
            slots: self.rack.chain.clone(),
        });
        self.rack.save();
    }

    pub(crate) fn delete_chain_preset(&mut self, name: &str) {
        self.rack.chain_presets.retain(|preset| preset.name != name);
        self.rack.save();
    }

    pub(crate) fn apply_effect_preset(&mut self, id: &str, name: &str) {
        let Some(kind) = self
            .rack
            .chain
            .iter()
            .find(|slot| slot.id == id)
            .map(|slot| slot.effect.kind())
        else {
            return;
        };
        let Some(effect) = effect_from_preset(&self.rack, kind, name) else {
            return;
        };
        self.edit_slot(id, true, |slot| {
            slot.enabled = true;
            slot.effect = effect;
        });
    }

    pub(crate) fn save_effect_preset(&mut self, id: &str, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        let Some(slot) = self.rack.chain.iter().find(|slot| slot.id == id).cloned() else {
            return;
        };
        match slot.effect {
            EffectSettings::Equalizer(eq) => {
                self.rack.eq_presets.retain(|preset| preset.name != name);
                self.rack.eq_presets.push(EqPreset {
                    name,
                    preamp_db: eq.preamp_db,
                    bands_db: eq.bands_db.to_vec(),
                });
            }
            EffectSettings::Filter(filter) => {
                self.rack.filter_presets.retain(|preset| preset.name != name);
                self.rack.filter_presets.push(FilterPreset {
                    name,
                    lp_hz: filter.lp_hz,
                    hp_hz: filter.hp_hz,
                    resonance: filter.resonance,
                });
            }
            EffectSettings::Limiter(limiter) => {
                self.rack.limiter_presets.retain(|preset| preset.name != name);
                self.rack.limiter_presets.push(LimiterPreset {
                    name,
                    gain_db: limiter.gain_db,
                    ceiling_db: limiter.ceiling_db,
                    release_ms: limiter.release_ms,
                    clip: limiter.clip,
                });
            }
        }
        self.rack.save();
    }

    pub(crate) fn delete_effect_preset(&mut self, kind: EffectKind, name: &str) {
        match kind {
            EffectKind::Equalizer => self.rack.eq_presets.retain(|preset| preset.name != name),
            EffectKind::Filter => self.rack.filter_presets.retain(|preset| preset.name != name),
            EffectKind::Limiter => {
                self.rack.limiter_presets.retain(|preset| preset.name != name)
            }
        }
        self.rack.save();
    }

    fn push_chain(&mut self, save: bool) {
        self.rack.chain.truncate(MAX_SLOTS);
        let chain = self.rack.chain.clone();
        if let Some(engine) = &self.engine {
            if let Err(error) = engine.apply_chain(chain) {
                self.push_notice(format!("DSP chain was not applied: {error}"));
            }
        }
        if save {
            self.rack.save();
        }
    }

    fn edit_slot(&mut self, id: &str, save: bool, edit: impl FnOnce(&mut ChainSlotSettings)) {
        if let Some(slot) = self.rack.chain.iter_mut().find(|slot| slot.id == id) {
            edit(slot);
            slot.effect = slot.effect.clone().clamp();
        } else {
            return;
        }
        self.push_chain(save);
    }
}

impl SettingsWindow {
    pub(super) fn poll_meters(&mut self, cx: &mut Context<Self>) -> bool {
        if self.section != SettingsSection::Audio {
            return false;
        }
        let expanded = self.audio.expanded.clone();
        let (watching, readings) = {
            let session = self.session.read(cx);
            let watching: Vec<(String, bool)> = session
                .rack
                .chain
                .iter()
                .filter_map(|slot| {
                    if !expanded.contains(&slot.id)
                        || !matches!(slot.effect, EffectSettings::Limiter(_))
                    {
                        return None;
                    }
                    Some((slot.id.clone(), slot.enabled))
                })
                .collect();
            let readings = session
                .engine
                .as_ref()
                .map(|engine| engine.limiter_readings())
                .unwrap_or_default();
            (watching, readings)
        };
        if watching.is_empty() {
            let had = !self.audio.meters.is_empty();
            self.audio.meters.clear();
            return had;
        }
        for (id, enabled) in &watching {
            let target = if *enabled {
                readings
                    .iter()
                    .find(|(slot_id, _)| slot_id == id)
                    .map(|(_, db)| *db)
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            let prev = self.audio.meters.get(id).copied().unwrap_or(0.0);
            let value = if target > prev {
                target
            } else {
                prev + (target - prev) * 0.35
            };
            self.audio.meters.insert(id.clone(), value);
        }
        self.audio
            .meters
            .retain(|id, _| watching.iter().any(|(slot_id, _)| slot_id == id));
        true
    }

    pub(super) fn audio_drag_move(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.audio.drag.clone() else {
            return;
        };
        match drag {
            Drag::Reorder {
                origin_y, active, ..
            } => {
                let active = active || (position.y.as_f32() - origin_y).abs() > 4.0;
                if let Some(Drag::Reorder { active: flag, .. }) = self.audio.drag.as_mut() {
                    *flag = active;
                }
                if active {
                    let ids: Vec<String> = self
                        .session
                        .read(cx)
                        .rack
                        .chain
                        .iter()
                        .map(|slot| slot.id.clone())
                        .collect();
                    self.audio.drop_at = Some(self.drop_index(position.y, &ids));
                }
                cx.notify();
            }
            Drag::Knob {
                id,
                knob,
                vertical,
            } => {
                let Some(bounds) = self.audio.bounds.get(&id).copied() else {
                    return;
                };
                let frac = if vertical {
                    frac_top(bounds, position.y)
                } else {
                    frac_left(bounds, position.x)
                };
                self.session.update(cx, |session, cx| {
                    apply_knob(session, &knob, frac);
                    cx.notify();
                });
            }
        }
    }

    pub(super) fn end_audio_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.audio.drag.take() else {
            return;
        };
        let drop_at = self.audio.drop_at.take();
        match drag {
            Drag::Reorder { id, active, .. } => {
                if active {
                    if let Some(at) = drop_at {
                        self.session.update(cx, |session, cx| {
                            session.move_effect(&id, at);
                            cx.notify();
                        });
                    }
                }
            }
            Drag::Knob { knob: Knob::Rate, .. } => {
                self.session.update(cx, |session, cx| {
                    session.commit_rate();
                    cx.notify();
                });
            }
            Drag::Knob { .. } => {
                self.session.update(cx, |session, cx| {
                    session.commit_chain();
                    cx.notify();
                });
            }
        }
    }

    pub(super) fn audio_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let rack = self.session.read(cx).rack.clone();
        let full = rack.chain.len() >= MAX_SLOTS;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, slot) in rack.chain.iter().enumerate() {
            if self.audio.drop_at == Some(index) {
                rows.push(drop_line().into_any_element());
            }
            rows.push(self.slot_row(index, slot, &rack, cx));
        }
        if self.audio.drop_at == Some(rack.chain.len()) && !rack.chain.is_empty() {
            rows.push(drop_line().into_any_element());
        }

        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(section_heading(
                "Audio",
                "Build an effect chain — drag effects in, stack them in any order, tune each one.",
            ))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .child(self.chain_card(&rack, rows, cx).flex_1().min_w(px(0.)))
                    .child(self.catalog_card(full, cx).w(px(220.)).flex_shrink_0()),
            )
            .child(self.rate_card(&rack, cx))
            .into_any_element()
    }

    fn chain_card(&self, rack: &AudioRack, rows: Vec<AnyElement>, cx: &mut Context<Self>) -> gpui::Div {
        let count = rack.chain.len();
        card()
            .py(px(10.))
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_size(px(13.)).child("Chain"))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme::text_muted())
                            .child(format!("{count}/{MAX_SLOTS}")),
                    )
                    .child(div().flex_1())
                    .when(count > 0, |row| {
                        let clear = bind_click("chain-clear", cx, |this, _, cx| {
                            this.audio.expanded.clear();
                            this.audio.menu = Menu::Closed;
                            this.session.update(cx, |session, cx| {
                                session.clear_chain();
                                cx.notify();
                            });
                            cx.notify();
                        });
                        row.child(text_button(clear, "Clear"))
                    })
                    .child(self.preset_button(
                        "chain-preset",
                        &chain_preset_label(rack),
                        matches!(self.audio.menu, Menu::Chain { .. }),
                        cx,
                        |this, _, cx| {
                            this.audio.menu = match this.audio.menu {
                                Menu::Chain { saving: false } => Menu::Closed,
                                _ => Menu::Chain { saving: false },
                            };
                            cx.notify();
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .children(rows)
                    .when(count == 0, |list| {
                        list.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .py(px(12.))
                                .child(div().text_size(px(13.)).child("The chain is empty"))
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(theme::text_muted())
                                        .child(
                                            "Add an effect from the right. Audio runs through the chain top to bottom.",
                                        ),
                                ),
                        )
                    }),
            )
    }

    fn chain_menu(&self, rack: &AudioRack, cx: &mut Context<Self>) -> AnyElement {
        let Menu::Chain { saving } = &self.audio.menu else {
            return div().into_any_element();
        };
        if *saving {
            return self.save_editor(cx);
        }
        let mut items: Vec<AnyElement> = Vec::new();
        items.push(
            crate::ui::menu::row(
                bind_click("chain-save-as", cx, |this, window, cx| {
                    this.audio.menu = Menu::Chain { saving: true };
                    this.prepare_name(window, cx);
                }),
                crate::ui::menu::Tone::Accent,
            )
            .child("Save current chain as…")
            .into_any_element(),
        );
        let current = chain_preset_label(rack);
        for preset in &rack.chain_presets {
            items.push(self.preset_row(
                "chain",
                &preset.name,
                current == preset.name,
                cx,
                {
                    let name = preset.name.clone();
                    move |this, _, cx| {
                        this.audio.expanded.clear();
                        this.audio.menu = Menu::Closed;
                        this.session.update(cx, |session, cx| {
                            session.apply_chain_preset(&name);
                            cx.notify();
                        });
                        cx.notify();
                    }
                },
                {
                    let name = preset.name.clone();
                    move |this, _, cx| {
                        this.session.update(cx, |session, cx| {
                            session.delete_chain_preset(&name);
                            cx.notify();
                        });
                        cx.notify();
                    }
                },
            ));
        }
        div().flex().flex_col().gap(px(2.)).children(items).into_any_element()
    }

    fn catalog_card(&self, full: bool, cx: &mut Context<Self>) -> gpui::Div {
        const CATALOG: [(EffectKind, &str, &str); 3] = [
            (EffectKind::Equalizer, "Equalizer", "17-band graphic EQ with preamp"),
            (EffectKind::Filter, "Filter", "Resonant low-pass / high-pass"),
            (EffectKind::Limiter, "Limiter", "Brickwall ceiling, or hard clip"),
        ];
        let mut card = card().py(px(10.)).gap(px(6.)).child(
            div()
                .text_size(px(13.))
                .pb(px(4.))
                .child("Available"),
        );
        for (kind, label, blurb) in CATALOG {
            let id = format!("add-{}", kind_name(kind));
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .py(px(4.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(if full { theme::text_muted() } else { theme::text() })
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme::text_muted())
                                    .child(blurb),
                            ),
                    )
                    .child(if full {
                        div()
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(theme::text_muted())
                            .child("+")
                            .into_any_element()
                    } else {
                        bind_click(id, cx, move |this, _, cx| {
                            let mut added = None;
                            this.session.update(cx, |session, cx| {
                                added = session.add_effect(kind);
                                cx.notify();
                            });
                            if let Some(id) = added {
                                this.audio.expanded.insert(id);
                            }
                            cx.notify();
                        })
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .text_size(px(16.))
                        .hover(|style| style.bg(theme::hover()))
                        .child("+")
                        .into_any_element()
                    }),
            );
        }
        card
    }

    fn rate_card(&self, rack: &AudioRack, cx: &mut Context<Self>) -> gpui::Div {
        let rate = rack.playback_rate;
        let frac = ((rate - 0.25) / 1.75).clamp(0.0, 1.0);
        card().py(px(12.)).gap(px(10.)).child(
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .child(div().text_size(px(13.)).child("Playback speed"))
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(theme::text_muted())
                                        .child(if rack.pitch_enabled {
                                            "Speed changes shift pitch (vinyl-style)"
                                        } else {
                                            "Original pitch preserved while changing speed"
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(18.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(format!("{rate:.2}×")),
                        ),
                )
                .child(self.h_slider("rate", frac, Knob::Rate, cx))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .child("0.25×")
                        .child("2.00×"),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .children(RATE_PRESETS.map(|preset| {
                            let active = (rate - preset).abs() < 0.01;
                            let label = if (preset - 1.0).abs() < 0.001 {
                                "1.0×".to_string()
                            } else {
                                format!("{preset:.2}×")
                            };
                            text_button(
                                bind_click(format!("rate-{preset}"), cx, move |this, _, cx| {
                                    this.session.update(cx, |session, cx| {
                                        session.preview_rate(preset);
                                        session.commit_rate();
                                        cx.notify();
                                    });
                                    cx.notify();
                                })
                                .when(active, |button| button.bg(theme::bg_elevated())),
                                label,
                            )
                            .into_any_element()
                        }))
                        .child(text_button(
                            bind_click("pitch-toggle", cx, |this, _, cx| {
                                let on = !this.session.read(cx).rack.pitch_enabled;
                                this.session.update(cx, |session, cx| {
                                    session.set_pitch_coupled(on);
                                    cx.notify();
                                });
                                cx.notify();
                            })
                            .when(rack.pitch_enabled, |button| button.bg(theme::accent_soft())),
                            "Pitch",
                        )),
                ),
        )
    }

    fn slot_row(
        &self,
        index: usize,
        slot: &ChainSlotSettings,
        rack: &AudioRack,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.audio.expanded.contains(&slot.id);
        let lifted = matches!(
            &self.audio.drag,
            Some(Drag::Reorder { id, active: true, .. }) if id == &slot.id
        );
        let kind = slot.effect.kind();
        let slot_id = slot.id.clone();
        let summary = slot_summary(slot);
        let label = effect_label(kind);

        div()
            .relative()
            .rounded_md()
            .bg(theme::bg_surface())
            .mb(px(6.))
            .when(lifted, |row| row.opacity(0.45))
            .child(self.bounds_canvas(format!("row|{}", slot.id), cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(6.))
                    .py(px(6.))
                    .child(self.grip(&slot.id, cx))
                    .child(
                        div()
                            .w(px(16.))
                            .text_size(px(12.))
                            .text_color(theme::text_muted())
                            .child(format!("{}", index + 1)),
                    )
                    .child(self.power_switch(&slot.id, slot.enabled, cx))
                    .child(
                        bind_click(format!("open|{}", slot.id), cx, {
                            let id = slot_id.clone();
                            move |this, _, cx| {
                                if !this.audio.expanded.remove(&id) {
                                    this.audio.expanded.insert(id.clone());
                                }
                                cx.notify();
                            }
                        })
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(if slot.enabled { theme::text() } else { theme::text_muted() })
                                .child(label),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme::text_muted())
                                .child(summary),
                        ),
                    )
                    .child(self.nudge_button(&slot.id, -1, index > 0, cx))
                    .child(self.nudge_button(&slot.id, 1, index + 1 < rack.chain.len(), cx))
                    .child(
                        bind_click(format!("remove|{}", slot.id), cx, {
                            let id = slot_id.clone();
                            move |this, _, cx| {
                                this.audio.expanded.remove(&id);
                                this.session.update(cx, |session, cx| {
                                    session.remove_effect(&id);
                                    cx.notify();
                                });
                                cx.notify();
                            }
                        })
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .hover(|style| style.bg(theme::hover()))
                        .child(icon(icons::CLOSE, theme::text_muted(), px(10.))),
                    ),
            )
            .when(open, |row| row.child(self.slot_body(slot, rack, cx)))
            .into_any_element()
    }

    fn slot_body(&self, slot: &ChainSlotSettings, rack: &AudioRack, cx: &mut Context<Self>) -> AnyElement {
        div()
            .px(px(8.))
            .pb(px(8.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(self.effect_toolbar(slot, rack, cx))
            .child(match &slot.effect {
                EffectSettings::Equalizer(eq) => self.equalizer(slot, eq, cx),
                EffectSettings::Filter(filter) => self.filter(slot, filter, cx),
                EffectSettings::Limiter(limiter) => self.limiter(slot, limiter, cx),
            })
            .into_any_element()
    }

    fn effect_toolbar(
        &self,
        slot: &ChainSlotSettings,
        rack: &AudioRack,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = slot.id.clone();
        let open = matches!(&self.audio.menu, Menu::Effect { slot: open_id, .. } if open_id == &slot.id);
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(text_button(
                        bind_click(format!("reset|{}", slot.id), cx, {
                            let id = id.clone();
                            move |this, _, cx| {
                                this.session.update(cx, |session, cx| {
                                    session.reset_slot(&id);
                                    cx.notify();
                                });
                                cx.notify();
                            }
                        }),
                        "Reset",
                    ))
                    .child(self.preset_button(
                        format!("effect-preset|{}", slot.id),
                        &effect_preset_label(rack, slot),
                        open,
                        cx,
                        {
                            let id = id.clone();
                            move |this, _, cx| {
                                this.audio.menu = match &this.audio.menu {
                                    Menu::Effect {
                                        slot: open_id,
                                        saving: false,
                                    } if open_id == &id => Menu::Closed,
                                    _ => Menu::Effect {
                                        slot: id.clone(),
                                        saving: false,
                                    },
                                };
                                cx.notify();
                            }
                        },
                    )),
            )
            .into_any_element()
    }

    fn effect_menu(&self, slot: &ChainSlotSettings, rack: &AudioRack, cx: &mut Context<Self>) -> AnyElement {
        let Menu::Effect { slot: open_id, saving } = &self.audio.menu else {
            return div().into_any_element();
        };
        if open_id != &slot.id {
            return div().into_any_element();
        }
        if *saving {
            return self.save_editor(cx);
        }
        let kind = slot.effect.kind();
        let names = preset_names(rack, kind);
        let current = effect_preset_label(rack, slot);
        let mut items: Vec<AnyElement> = Vec::new();
        let slot_id = slot.id.clone();
        items.push(
            crate::ui::menu::row(
                bind_click(format!("effect-save|{}", slot.id), cx, {
                    let slot_id = slot_id.clone();
                    move |this, window, cx| {
                        this.audio.menu = Menu::Effect {
                            slot: slot_id.clone(),
                            saving: true,
                        };
                        this.prepare_name(window, cx);
                    }
                }),
                crate::ui::menu::Tone::Accent,
            )
            .child("Save current as…")
            .into_any_element(),
        );
        for name in names {
            items.push(self.preset_row(
                &format!("fx|{}", slot.id),
                &name,
                current == name,
                cx,
                {
                    let slot_id = slot_id.clone();
                    let name = name.clone();
                    move |this, _, cx| {
                        this.audio.menu = Menu::Closed;
                        this.session.update(cx, |session, cx| {
                            session.apply_effect_preset(&slot_id, &name);
                            cx.notify();
                        });
                        cx.notify();
                    }
                },
                {
                    let name = name.clone();
                    move |this, _, cx| {
                        this.session.update(cx, |session, cx| {
                            session.delete_effect_preset(kind, &name);
                            cx.notify();
                        });
                        cx.notify();
                    }
                },
            ));
        }
        div().flex().flex_col().gap(px(2.)).children(items).into_any_element()
    }

    fn equalizer(&self, slot: &ChainSlotSettings, eq: &EqualizerSettings, cx: &mut Context<Self>) -> AnyElement {
        let mut bands: Vec<AnyElement> = Vec::new();
        let pre_frac = ((eq.preamp_db + 15.0) / 30.0).clamp(0.0, 1.0);
        bands.push(self.band_column(
            "Pre",
            &db_text(eq.preamp_db),
            self.v_slider(format!("pre|{}", slot.id), pre_frac, Knob::Preamp { slot: slot.id.clone() }, cx),
        ));
        for index in 0..BAND_COUNT {
            let gain = eq.bands_db[index];
            let frac = ((gain + 20.0) / 40.0).clamp(0.0, 1.0);
            bands.push(self.band_column(
                &format_hz(BAND_FREQUENCIES[index]),
                &db_text(gain),
                self.v_slider(
                    format!("band|{index}|{}", slot.id),
                    frac,
                    Knob::Band { slot: slot.id.clone(), index },
                    cx,
                ),
            ));
        }
        div()
            .id("eq-bands")
            .w_full()
            .overflow_x_scroll()
            .flex()
            .gap(px(2.))
            .children(bands)
            .into_any_element()
    }

    fn band_column(&self, freq: &str, gain: &str, slider: AnyElement) -> AnyElement {
        div()
            .w(px(36.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .child(slider)
            .child(div().text_size(px(10.)).child(gain.to_string()))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(theme::text_muted())
                    .child(freq.to_string()),
            )
            .into_any_element()
    }

    fn filter(&self, slot: &ChainSlotSettings, filter: &FilterSettings, cx: &mut Context<Self>) -> AnyElement {
        let hp_open = filter.hp_hz <= HP_OPEN_HZ + 0.5;
        let lp_open = filter.lp_hz >= LP_OPEN_HZ - 0.5;
        let inverted = !hp_open && !lp_open && filter.hp_hz >= filter.lp_hz;
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.labeled_slider(
                "High-pass",
                &if hp_open { "Open".to_string() } else { format!("{} Hz", format_hz(filter.hp_hz)) },
                "Open",
                "20k",
                self.h_slider(
                    format!("hp|{}", slot.id),
                    hz_to_pos(filter.hp_hz),
                    Knob::Highpass { slot: slot.id.clone() },
                    cx,
                ),
            ))
            .child(self.labeled_slider(
                "Low-pass",
                &if lp_open { "Open".to_string() } else { format!("{} Hz", format_hz(filter.lp_hz)) },
                "20",
                "Open",
                self.h_slider(
                    format!("lp|{}", slot.id),
                    hz_to_pos(filter.lp_hz),
                    Knob::Lowpass { slot: slot.id.clone() },
                    cx,
                ),
            ))
            .child(self.labeled_slider(
                "Resonance",
                &format!("Q {:.2}", filter.resonance),
                "Flat",
                "Squelch",
                self.h_slider(
                    format!("q|{}", slot.id),
                    ((filter.resonance - 0.5) / 7.5).clamp(0.0, 1.0),
                    Knob::Resonance { slot: slot.id.clone() },
                    cx,
                ),
            ))
            .when(inverted, |column| {
                column.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::danger())
                        .child("High-pass is above low-pass — nothing is left in the passband."),
                )
            })
            .into_any_element()
    }

    fn limiter(&self, slot: &ChainSlotSettings, limiter: &LimiterSettings, cx: &mut Context<Self>) -> AnyElement {
        let reduction = self.audio.meters.get(&slot.id).copied().unwrap_or(0.0).max(0.0);
        let meter = (reduction / 12.0).clamp(0.0, 1.0);
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(self.labeled_slider(
                "Gain",
                &format!("+{:.1} dB", limiter.gain_db),
                "0 dB",
                "+12 dB",
                self.h_slider(
                    format!("gain|{}", slot.id),
                    (limiter.gain_db / 12.0).clamp(0.0, 1.0),
                    Knob::Gain { slot: slot.id.clone() },
                    cx,
                ),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(12.)).child("Hard clip"))
                    .child(self.bool_switch(
                        format!("clip|{}", slot.id),
                        limiter.clip,
                        {
                            let id = slot.id.clone();
                            move |this, _, cx| {
                                let clip = this
                                    .session
                                    .read(cx)
                                    .rack
                                    .chain
                                    .iter()
                                    .find(|slot| slot.id == id)
                                    .and_then(|slot| match &slot.effect {
                                        EffectSettings::Limiter(limiter) => Some(!limiter.clip),
                                        _ => None,
                                    })
                                    .unwrap_or(false);
                                let id = id.clone();
                                this.session.update(cx, |session, cx| {
                                    session.set_limiter_clip(&id, clip);
                                    cx.notify();
                                });
                            }
                        },
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(12.))
                            .child(if limiter.clip { "Clipped" } else { "Gain reduction" })
                            .child(format!("−{reduction:.1} dB")),
                    )
                    .child(
                        div()
                            .h(px(4.))
                            .w_full()
                            .rounded_full()
                            .bg(theme::bg_elevated())
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(meter))
                                    .rounded_full()
                                    .bg(if limiter.clip { theme::danger() } else { theme::accent() }),
                            ),
                    ),
            )
            .child(self.labeled_slider(
                "Ceiling",
                &format!("{:.1} dBFS", limiter.ceiling_db),
                "−6",
                "0",
                self.h_slider(
                    format!("ceiling|{}", slot.id),
                    ((limiter.ceiling_db + 6.0) / 6.0).clamp(0.0, 1.0),
                    Knob::Ceiling { slot: slot.id.clone() },
                    cx,
                ),
            ))
            .child(self.labeled_slider(
                "Release",
                &format!("{:.0} ms", limiter.release_ms),
                "10 ms",
                "1000 ms",
                self.h_slider(
                    format!("release|{}", slot.id),
                    ((limiter.release_ms - 10.0) / 990.0).clamp(0.0, 1.0),
                    Knob::Release { slot: slot.id.clone() },
                    cx,
                ),
            ))
            .into_any_element()
    }

    fn labeled_slider(
        &self,
        label: &'static str,
        value: &str,
        low: &'static str,
        high: &'static str,
        slider: AnyElement,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(12.))
                    .child(label)
                    .child(div().text_color(theme::text_muted()).child(value.to_string())),
            )
            .child(slider)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(11.))
                    .text_color(theme::text_muted())
                    .child(low)
                    .child(high),
            )
            .into_any_element()
    }

    fn h_slider(
        &self,
        id: impl Into<String>,
        frac: f32,
        knob: Knob,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.slider(id.into(), frac, knob, false, px(16.), cx)
    }

    fn v_slider(&self, id: String, frac: f32, knob: Knob, cx: &mut Context<Self>) -> AnyElement {
        let top = 1.0 - frac;
        let (fill_top, fill_h) = if frac >= 0.5 {
            (top, frac - 0.5)
        } else {
            (0.5, 0.5 - frac)
        };
        let id_down = id.clone();
        let knob_down = knob.clone();
        div()
            .relative()
            .w(px(28.))
            .h(px(112.))
            .cursor_pointer()
            .child(self.bounds_canvas(id.clone(), cx))
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .left(px(12.))
                    .w(px(4.))
                    .h_full()
                    .rounded_full()
                    .bg(theme::bg_elevated()),
            )
            .child(
                div()
                    .absolute()
                    .left(px(12.))
                    .w(px(4.))
                    .top(relative(fill_top))
                    .h(relative(fill_h))
                    .rounded_full()
                    .bg(theme::accent()),
            )
            .child(
                div()
                    .absolute()
                    .left(px(9.))
                    .top(relative(top))
                    .mt(px(-5.))
                    .size(px(10.))
                    .rounded_full()
                    .bg(theme::white()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.begin_knob(id_down.clone(), knob_down.clone(), true, event.position, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.end_audio_drag(cx);
                }),
            )
            .into_any_element()
    }

    fn slider(
        &self,
        id: String,
        frac: f32,
        knob: Knob,
        _vertical: bool,
        height: gpui::Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id_down = id.clone();
        let knob_down = knob;
        div()
            .id(SharedString::from(id.clone()))
            .relative()
            .w_full()
            .h(height)
            .cursor_pointer()
            .child(self.bounds_canvas(id, cx))
            .child(
                div()
                    .absolute()
                    .left(px(0.))
                    .right(px(0.))
                    .top(px(6.))
                    .h(px(4.))
                    .rounded_full()
                    .bg(theme::bg_elevated())
                    .child(div().h_full().w(relative(frac)).rounded_full().bg(theme::accent())),
            )
            .child(
                div()
                    .absolute()
                    .top(px(3.))
                    .left(relative(frac))
                    .ml(px(-5.))
                    .size(px(10.))
                    .rounded_full()
                    .bg(theme::white()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    this.begin_knob(id_down.clone(), knob_down.clone(), false, event.position, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.end_audio_drag(cx);
                }),
            )
            .into_any_element()
    }

    fn bounds_canvas(&self, id: String, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity().clone();
        canvas(
            move |bounds, _window, app| {
                this.update(app, |view, _cx| {
                    view.audio.bounds.insert(id.clone(), bounds);
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.))
        .into_any_element()
    }

    fn grip(&self, slot_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let id = slot_id.to_string();
        div()
            .id(SharedString::from(format!("grip|{slot_id}")))
            .px(px(2.))
            .cursor_pointer()
            .text_size(px(14.))
            .text_color(theme::text_muted())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.begin_reorder(id.clone(), event.position.y);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseUpEvent, _, cx| {
                    this.end_audio_drag(cx);
                }),
            )
            .child("⠿")
            .into_any_element()
    }

    fn power_switch(&self, slot_id: &str, on: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = slot_id.to_string();
        bind_click(format!("power|{slot_id}"), cx, move |this, _, cx| {
            let enabled = !on;
            this.session.update(cx, |session, cx| {
                session.set_slot_power(&id, enabled);
                cx.notify();
            });
        })
        .w(px(36.))
        .h(px(20.))
        .rounded_full()
        .p(px(2.))
        .flex_shrink_0()
        .bg(if on { theme::accent() } else { theme::bg_elevated() })
        .child(
            div()
                .size(px(16.))
                .rounded_full()
                .bg(theme::white())
                .when(on, |knob| knob.ml(px(16.))),
        )
        .into_any_element()
    }

    fn bool_switch(
        &self,
        id: String,
        on: bool,
        action: impl Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        bind_click(id, cx, action)
            .w(px(36.))
            .h(px(20.))
            .rounded_full()
            .p(px(2.))
            .bg(if on { theme::accent() } else { theme::bg_elevated() })
            .child(
                div()
                    .size(px(16.))
                    .rounded_full()
                    .bg(theme::white())
                    .when(on, |knob| knob.ml(px(16.))),
            )
            .into_any_element()
    }

    fn nudge_button(&self, slot_id: &str, dir: i32, enabled: bool, cx: &mut Context<Self>) -> AnyElement {
        let mark = if dir < 0 { "↑" } else { "↓" };
        if !enabled {
            return div()
                .size(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(theme::text_muted())
                .opacity(0.35)
                .child(mark)
                .into_any_element();
        }
        let id = slot_id.to_string();
        bind_click(format!("nudge|{dir}|{slot_id}"), cx, move |this, _, cx| {
            this.session.update(cx, |session, cx| {
                session.nudge_effect(&id, dir);
                cx.notify();
            });
        })
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .text_size(px(12.))
        .hover(|style| style.bg(theme::hover()))
        .child(mark)
        .into_any_element()
    }

    fn preset_button(
        &self,
        id: impl Into<SharedString>,
        label: &str,
        open: bool,
        cx: &mut Context<Self>,
        action: impl Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>) + 'static,
    ) -> AnyElement {
        let id = id.into();
        let anchor = format!("anchor|{id}");
        div()
            .relative()
            .child(self.anchor_probe(anchor, cx))
            .child(text_button(
                bind_click(id, cx, action).when(open, |button| button.bg(theme::bg_elevated())),
                format!("Preset: {label}"),
            ))
            .into_any_element()
    }

    fn anchor_probe(&self, key: String, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity().clone();
        canvas(
            move |bounds, _, cx| {
                entity.update(cx, |this, cx| {
                    let changed = this
                        .audio
                        .bounds
                        .get(&key)
                        .map(|old| {
                            old.origin.x != bounds.origin.x
                                || old.origin.y != bounds.origin.y
                                || old.size.width != bounds.size.width
                                || old.size.height != bounds.size.height
                        })
                        .unwrap_or(true);
                    if !changed {
                        return;
                    }
                    this.audio.bounds.insert(key.clone(), bounds);
                    if !matches!(this.audio.menu, Menu::Closed) {
                        cx.notify();
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    pub(super) fn preset_popup(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.section != SettingsSection::Audio {
            return div().into_any_element();
        }
        let open = match &self.audio.menu {
            Menu::Closed => return div().into_any_element(),
            Menu::Chain { .. } => None,
            Menu::Effect { slot, .. } => Some(slot.clone()),
        };
        let rack = self.session.read(cx).rack.clone();
        let (key, body) = if let Some(slot) = open {
            let Some(slot_settings) = rack.chain.iter().find(|item| item.id == slot).cloned() else {
                return div().into_any_element();
            };
            (
                format!("effect-preset|{slot}"),
                self.effect_menu(&slot_settings, &rack, cx),
            )
        } else {
            ("chain-preset".to_string(), self.chain_menu(&rack, cx))
        };
        let Some(anchor) = self.audio.bounds.get(&format!("anchor|{key}")).copied() else {
            return div().into_any_element();
        };
        div()
            .absolute()
            .size_full()
            .child(
                crate::ui::menu::backdrop("preset-backdrop")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.armed = None;
                            this.audio.menu = Menu::Closed;
                            cx.notify();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, _, cx| {
                            this.armed = None;
                            this.audio.menu = Menu::Closed;
                            cx.notify();
                        }),
                    ),
            )
            .child(crate::ui::menu::under(
                anchor,
                true,
                crate::ui::menu::panel("preset-dropdown").child(body),
            ))
            .into_any_element()
    }

    fn preset_row(
        &self,
        prefix: &str,
        name: &str,
        active: bool,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>) + 'static,
        delete: impl Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>) + 'static,
    ) -> AnyElement {
        crate::ui::menu::row(
            bind_click(format!("{prefix}|apply|{name}"), cx, apply).w_full(),
            if active {
                crate::ui::menu::Tone::Active
            } else {
                crate::ui::menu::Tone::Normal
            },
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .truncate()
                .child(name.to_string()),
        )
        .child(
            bind_click(format!("{prefix}|delete|{name}"), cx, delete)
                .occlude()
                .flex_shrink(0.)
                .size(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .text_size(px(13.))
                .text_color(theme::text_muted())
                .hover(|style| {
                    style
                        .text_color(gpui::rgb(0xf87171))
                        .bg(gpui::rgba(0xf871711f))
                })
                .child("×"),
        )
        .into_any_element()
    }

    fn save_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .flex_1()
                    .h(px(28.))
                    .px(px(8.))
                    .rounded_md()
                    .bg(theme::bg_elevated())
                    .child(self.preset_name.clone()),
            )
            .child(text_button(
                bind_click("preset-confirm", cx, |this, _, cx| this.confirm_preset_save(cx)),
                "Save",
            ))
            .child(text_button(
                bind_click("preset-cancel", cx, |this, _, cx| {
                    this.audio.menu = Menu::Closed;
                    cx.notify();
                }),
                "Cancel",
            ))
            .into_any_element()
    }

    fn prepare_name(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.preset_name.update(cx, |field, cx| {
            field.set_text("", window, cx);
        });
        let handle = self.preset_name.read(cx).focus.clone();
        window.focus(&handle, cx);
        cx.notify();
    }

    fn confirm_preset_save(&mut self, cx: &mut Context<Self>) {
        let name = self.preset_name.read(cx).content.to_string();
        let menu = std::mem::replace(&mut self.audio.menu, Menu::Closed);
        self.session.update(cx, |session, cx| {
            match menu {
                Menu::Chain { .. } => session.save_chain_preset(&name),
                Menu::Effect { slot, .. } => session.save_effect_preset(&slot, &name),
                Menu::Closed => {}
            }
            cx.notify();
        });
        cx.notify();
    }

    fn begin_knob(
        &mut self,
        id: String,
        knob: Knob,
        vertical: bool,
        position: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.audio.drag = Some(Drag::Knob { id, knob, vertical });
        self.audio_drag_move(position, cx);
    }

    fn begin_reorder(&mut self, id: String, y: Pixels) {
        self.audio.drag = Some(Drag::Reorder {
            id,
            origin_y: y.as_f32(),
            active: false,
        });
        self.audio.drop_at = None;
    }

    fn drop_index(&self, y: Pixels, ids: &[String]) -> usize {
        for (index, id) in ids.iter().enumerate() {
            let Some(bounds) = self.audio.bounds.get(&format!("row|{id}")) else {
                continue;
            };
            let mid = bounds.top().as_f32() + bounds.size.height.as_f32() * 0.5;
            if y.as_f32() < mid {
                return index;
            }
        }
        ids.len()
    }
}

fn bind_click(
    id: impl Into<SharedString>,
    cx: &mut Context<SettingsWindow>,
    action: impl Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    let arm_id = id.clone();
    let up_id = id.clone();
    div()
        .id(id)
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.arm(arm_id.clone());
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _: &gpui::MouseUpEvent, window, cx| {
                if this.armed(up_id.as_ref()) {
                    action(this, window, cx);
                }
            }),
        )
}

fn text_button(button: gpui::Stateful<gpui::Div>, label: impl Into<SharedString>) -> gpui::Stateful<gpui::Div> {
    button
        .h(px(26.))
        .px(px(8.))
        .flex()
        .items_center()
        .rounded_md()
        .text_size(px(12.))
        .hover(|style| style.bg(theme::hover()))
        .child(label.into())
}

fn drop_line() -> gpui::Div {
    div()
        .h(px(2.))
        .mx(px(8.))
        .mb(px(6.))
        .rounded_full()
        .bg(theme::accent())
}

fn apply_knob(session: &mut Session, knob: &Knob, frac: f32) {
    let frac = frac.clamp(0.0, 1.0);
    match knob {
        Knob::Rate => session.preview_rate(snap(0.25 + frac * 1.75, 0.01)),
        Knob::Preamp { slot } => session.set_eq_preamp(slot, snap(-15.0 + frac * 30.0, 0.1)),
        Knob::Band { slot, index } => {
            session.set_eq_band(slot, *index, snap(-20.0 + frac * 40.0, 0.1))
        }
        Knob::Highpass { slot } => session.set_filter_hp(slot, pos_to_hz(frac)),
        Knob::Lowpass { slot } => session.set_filter_lp(slot, pos_to_hz(frac)),
        Knob::Resonance { slot } => session.set_filter_q(slot, snap(0.5 + frac * 7.5, 0.05)),
        Knob::Gain { slot } => session.set_limiter_gain(slot, snap(frac * 12.0, 0.5)),
        Knob::Ceiling { slot } => session.set_limiter_ceiling(slot, snap(-6.0 + frac * 6.0, 0.1)),
        Knob::Release { slot } => session.set_limiter_release(slot, snap(10.0 + frac * 990.0, 5.0)),
    }
}

fn effect_from_preset(rack: &AudioRack, kind: EffectKind, name: &str) -> Option<EffectSettings> {
    match kind {
        EffectKind::Equalizer => {
            let preset = rack.eq_presets.iter().find(|preset| preset.name == name)?;
            let mut bands = [0.0; BAND_COUNT];
            for (index, gain) in preset.bands_db.iter().take(BAND_COUNT).enumerate() {
                bands[index] = *gain;
            }
            Some(EffectSettings::Equalizer(
                EqualizerSettings {
                    enabled: true,
                    preamp_db: preset.preamp_db,
                    bands_db: bands,
                }
                .clamp(),
            ))
        }
        EffectKind::Filter => {
            let preset = rack.filter_presets.iter().find(|preset| preset.name == name)?;
            Some(EffectSettings::Filter(
                FilterSettings {
                    enabled: true,
                    lp_hz: preset.lp_hz,
                    hp_hz: preset.hp_hz,
                    resonance: preset.resonance,
                }
                .clamp(),
            ))
        }
        EffectKind::Limiter => {
            let preset = rack.limiter_presets.iter().find(|preset| preset.name == name)?;
            Some(EffectSettings::Limiter(
                LimiterSettings {
                    enabled: true,
                    gain_db: preset.gain_db,
                    ceiling_db: preset.ceiling_db,
                    release_ms: preset.release_ms,
                    clip: preset.clip,
                }
                .clamp(),
            ))
        }
    }
}

fn fresh_effect(kind: EffectKind) -> EffectSettings {
    match kind {
        EffectKind::Equalizer => EffectSettings::Equalizer(EqualizerSettings {
            enabled: true,
            preamp_db: 0.0,
            bands_db: [0.0; BAND_COUNT],
        }),
        EffectKind::Filter => EffectSettings::Filter(FilterSettings {
            enabled: true,
            lp_hz: LP_OPEN_HZ,
            hp_hz: HP_OPEN_HZ,
            resonance: 0.707,
        }),
        EffectKind::Limiter => EffectSettings::Limiter(LimiterSettings {
            enabled: true,
            gain_db: 0.0,
            ceiling_db: -0.3,
            release_ms: 120.0,
            clip: false,
        }),
    }
}

fn mint_id(kind: EffectKind) -> String {
    static SEQ: AtomicU32 = AtomicU32::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    format!("{}-{ms:x}-{n:x}", kind_name(kind))
}

fn kind_name(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Equalizer => "equalizer",
        EffectKind::Filter => "filter",
        EffectKind::Limiter => "limiter",
    }
}

fn effect_label(kind: EffectKind) -> &'static str {
    match kind {
        EffectKind::Equalizer => "Equalizer",
        EffectKind::Filter => "Filter",
        EffectKind::Limiter => "Limiter",
    }
}

fn slot_summary(slot: &ChainSlotSettings) -> String {
    match &slot.effect {
        EffectSettings::Equalizer(eq) => {
            let touched = eq.bands_db.iter().filter(|band| band.abs() > 0.05).count();
            if touched == 0 && eq.preamp_db.abs() < 0.05 {
                return "Flat".to_string();
            }
            let mut parts = Vec::new();
            if eq.preamp_db.abs() >= 0.05 {
                parts.push(format!("{} dB pre", db_text(eq.preamp_db)));
            }
            if touched > 0 {
                let noun = if touched == 1 { "band" } else { "bands" };
                parts.push(format!("{touched} {noun}"));
            }
            parts.join(" · ")
        }
        EffectSettings::Filter(filter) => {
            let lp = filter.lp_hz < LP_OPEN_HZ;
            let hp = filter.hp_hz > HP_OPEN_HZ;
            if !lp && !hp {
                return "Open".to_string();
            }
            let mut parts = Vec::new();
            if hp {
                parts.push(format!("HP {}", format_hz(filter.hp_hz)));
            }
            if lp {
                parts.push(format!("LP {}", format_hz(filter.lp_hz)));
            }
            if filter.resonance > 0.75 {
                parts.push(format!("Q {:.1}", filter.resonance));
            }
            parts.join(" · ")
        }
        EffectSettings::Limiter(limiter) => {
            let head = if limiter.clip { "Clip" } else { "Limit" };
            format!(
                "{head} {:.1} dBFS · +{:.1} dB",
                limiter.ceiling_db, limiter.gain_db
            )
        }
    }
}

fn chain_preset_label(rack: &AudioRack) -> String {
    if let Some(preset) = rack
        .chain_presets
        .iter()
        .find(|preset| chains_match(&rack.chain, &preset.slots))
    {
        return preset.name.clone();
    }
    if rack.chain_presets.is_empty() {
        "None".to_string()
    } else {
        "Custom".to_string()
    }
}

fn effect_preset_label(rack: &AudioRack, slot: &ChainSlotSettings) -> String {
    let matched = match &slot.effect {
        EffectSettings::Equalizer(eq) => rack
            .eq_presets
            .iter()
            .find(|preset| eq_matches(eq, preset))
            .map(|preset| preset.name.clone()),
        EffectSettings::Filter(filter) => rack
            .filter_presets
            .iter()
            .find(|preset| {
                (preset.lp_hz - filter.lp_hz).abs() < 1.0
                    && (preset.hp_hz - filter.hp_hz).abs() < 1.0
                    && (preset.resonance - filter.resonance).abs() < 0.02
            })
            .map(|preset| preset.name.clone()),
        EffectSettings::Limiter(limiter) => rack
            .limiter_presets
            .iter()
            .find(|preset| {
                (preset.gain_db - limiter.gain_db).abs() < 0.05
                    && (preset.ceiling_db - limiter.ceiling_db).abs() < 0.05
                    && (preset.release_ms - limiter.release_ms).abs() < 1.0
                    && preset.clip == limiter.clip
            })
            .map(|preset| preset.name.clone()),
    };
    if let Some(name) = matched {
        return name;
    }
    let empty = match slot.effect.kind() {
        EffectKind::Equalizer => rack.eq_presets.is_empty(),
        EffectKind::Filter => rack.filter_presets.is_empty(),
        EffectKind::Limiter => rack.limiter_presets.is_empty(),
    };
    if empty { "None".to_string() } else { "Custom".to_string() }
}

fn preset_names(rack: &AudioRack, kind: EffectKind) -> Vec<String> {
    match kind {
        EffectKind::Equalizer => rack.eq_presets.iter().map(|preset| preset.name.clone()).collect(),
        EffectKind::Filter => rack.filter_presets.iter().map(|preset| preset.name.clone()).collect(),
        EffectKind::Limiter => rack
            .limiter_presets
            .iter()
            .map(|preset| preset.name.clone())
            .collect(),
    }
}

fn eq_matches(eq: &EqualizerSettings, preset: &EqPreset) -> bool {
    if (eq.preamp_db - preset.preamp_db).abs() >= 0.05 || preset.bands_db.len() != eq.bands_db.len() {
        return false;
    }
    preset
        .bands_db
        .iter()
        .zip(eq.bands_db.iter())
        .all(|(saved, live)| (saved - live).abs() < 0.05)
}

fn chains_match(live: &[ChainSlotSettings], saved: &[ChainSlotSettings]) -> bool {
    if live.len() != saved.len() {
        return false;
    }
    live.iter().zip(saved.iter()).all(|(left, right)| {
        if left.effect.kind() != right.effect.kind() || left.enabled != right.enabled {
            return false;
        }
        match (&left.effect, &right.effect) {
            (EffectSettings::Equalizer(a), EffectSettings::Equalizer(b)) => {
                (a.preamp_db - b.preamp_db).abs() < 0.05
                    && a.bands_db.iter().zip(b.bands_db.iter()).all(|(x, y)| (x - y).abs() < 0.05)
            }
            (EffectSettings::Filter(a), EffectSettings::Filter(b)) => {
                (a.lp_hz - b.lp_hz).abs() < 1.0
                    && (a.hp_hz - b.hp_hz).abs() < 1.0
                    && (a.resonance - b.resonance).abs() < 0.02
            }
            (EffectSettings::Limiter(a), EffectSettings::Limiter(b)) => {
                (a.gain_db - b.gain_db).abs() < 0.05
                    && (a.ceiling_db - b.ceiling_db).abs() < 0.05
                    && (a.release_ms - b.release_ms).abs() < 1.0
                    && a.clip == b.clip
            }
            _ => false,
        }
    })
}

fn format_hz(hz: f32) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{}k", k.round() as i32)
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{}", hz.round() as i32)
    }
}

fn db_text(value: f32) -> String {
    let shown = (value * 10.0).round() / 10.0;
    if shown > 0.0 {
        format!("+{shown:.1}")
    } else {
        format!("{shown:.1}")
    }
}

fn hz_to_pos(hz: f32) -> f32 {
    let hz = hz.clamp(HP_OPEN_HZ, LP_OPEN_HZ);
    let log_min = HP_OPEN_HZ.ln();
    let span = LP_OPEN_HZ.ln() - log_min;
    (hz.ln() - log_min) / span
}

fn pos_to_hz(pos: f32) -> f32 {
    if pos <= 0.005 {
        return HP_OPEN_HZ;
    }
    if pos >= 0.995 {
        return LP_OPEN_HZ;
    }
    let log_min = HP_OPEN_HZ.ln();
    let span = LP_OPEN_HZ.ln() - log_min;
    (log_min + pos.clamp(0.0, 1.0) * span).exp().round()
}

fn snap(value: f32, step: f32) -> f32 {
    if step <= 0.0 {
        return value;
    }
    let snapped = (value / step).round() * step;
    (snapped * 1000.0).round() / 1000.0
}

fn frac_left(bounds: Bounds<Pixels>, x: Pixels) -> f32 {
    let width = bounds.size.width;
    if width < px(1.) {
        return 0.0;
    }
    ((x - bounds.left()) / width).clamp(0.0, 1.0)
}

fn frac_top(bounds: Bounds<Pixels>, y: Pixels) -> f32 {
    let height = bounds.size.height;
    if height < px(1.) {
        return 0.0;
    }
    (1.0 - ((y - bounds.top()) / height)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_labels_match_the_web() {
        assert_eq!(format_hz(63.0), "63");
        assert_eq!(format_hz(1000.0), "1k");
        assert_eq!(format_hz(12500.0), "12.5k");
        assert_eq!(pos_to_hz(0.0), 20.0);
        assert_eq!(pos_to_hz(1.0), 20000.0);
    }

    #[test]
    fn flat_equalizer_reads_flat() {
        let slot = ChainSlotSettings {
            id: "eq".into(),
            enabled: true,
            effect: fresh_effect(EffectKind::Equalizer),
        };
        assert_eq!(slot_summary(&slot), "Flat");
    }

    #[test]
    fn adding_an_effect_does_not_touch_settings() {
        let mut session = Session::preview();
        let id = session.add_effect(EffectKind::Equalizer).unwrap();
        let filter = session.add_effect(EffectKind::Filter).unwrap();
        session.move_effect(&filter, 0);
        assert_eq!(session.rack.chain[0].id, filter);
        assert_eq!(session.rack.chain[1].id, id);
        assert!(!session.rack.persist);
        session.set_eq_band(&id, 0, 4.0);
        session.preview_rate(1.25);
        session.commit_rate();
    }
}
