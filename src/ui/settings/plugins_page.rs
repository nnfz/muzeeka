//! Settings → Plugins. The list is loaded when the section opens and about
//! once a second while it stays open. Text fields are created here, not during
//! render, so a refresh does not wipe text the user is still editing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{
    canvas, div, prelude::*, px, relative, AnyElement, Bounds, Context, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, SharedString, Styled,
};
use serde_json::{json, Value};

use super::SettingsWindow;
use crate::ui::plugins::{PluginInfo, PluginRuntime, PluginSettingSpec, SettingKind};
use crate::ui::session::SettingsSection;
use crate::ui::text_field::TextField;
use crate::ui::theme;

pub(super) struct PluginUi {
    list: Vec<PluginInfo>,
    error: Option<String>,
    fields: HashMap<String, gpui::Entity<TextField>>,
    synced: HashMap<String, String>,
    bounds: HashMap<String, Bounds<Pixels>>,
    pub(super) due: bool,
    refreshed: Instant,
}

impl PluginUi {
    pub(super) fn new() -> Self {
        Self {
            list: Vec::new(),
            error: None,
            fields: HashMap::new(),
            synced: HashMap::new(),
            bounds: HashMap::new(),
            due: false,
            refreshed: Instant::now(),
        }
    }
}

impl SettingsWindow {
    pub(super) fn refresh_plugins(&mut self, cx: &mut Context<Self>) -> bool {
        if self.section != SettingsSection::Plugins {
            return false;
        }
        let due = self.plugins.due || self.plugins.refreshed.elapsed() >= Duration::from_secs(1);
        if !due {
            return false;
        }
        self.plugins.due = false;
        self.plugins.refreshed = Instant::now();
        self.reload_plugins(cx);
        true
    }

    fn reload_plugins(&mut self, cx: &mut Context<Self>) {
        let list = crate::ui::plugins::list(cx);
        self.sync_plugin_fields(&list, cx);
        self.plugins.list = list;
        cx.notify();
    }

    fn sync_plugin_fields(&mut self, list: &[PluginInfo], cx: &mut Context<Self>) {
        let mut live = Vec::new();
        for plugin in list {
            for spec in &plugin.settings {
                if !matches!(spec.kind, SettingKind::String | SettingKind::Number) {
                    continue;
                }
                let draft_key = draft_key(&plugin.id, &spec.key);
                live.push(draft_key.clone());
                let desired = setting_text(&plugin.config, spec);
                if let Some(field) = self.plugins.fields.get(&draft_key).cloned() {
                    let current = field.read(cx).content.to_string();
                    let synced = self
                        .plugins
                        .synced
                        .get(&draft_key)
                        .map(String::as_str)
                        .unwrap_or("");
                    if current == synced && current != desired {
                        field.update(cx, |field, cx| field.replace_text(&desired, cx));
                        self.plugins.synced.insert(draft_key, desired);
                    }
                } else {
                    let field = make_plugin_field(&draft_key, &desired, cx);
                    self.plugins.fields.insert(draft_key.clone(), field);
                    self.plugins.synced.insert(draft_key, desired);
                }
            }
        }
        self.plugins.fields.retain(|key, _| live.iter().any(|live| live == key));
        self.plugins.synced.retain(|key, _| live.iter().any(|live| live == key));
    }

    pub(super) fn plugins_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let dir = crate::ui::plugins::plugins_dir().display().to_string();
        let error = self.plugins.error.clone();
        let list = self.plugins.list.clone();
        let cards: Vec<AnyElement> = list
            .iter()
            .map(|plugin| self.plugin_card(plugin, cx))
            .collect();

        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(super::section_heading(
                "Plugins",
                "Extensions loaded from the plugins folder.",
            ))
            .child(
                super::card().py(px(10.)).child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(16.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w(px(0.))
                                .gap(px(2.))
                                .child(div().text_size(px(13.)).child("Folder"))
                                .child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(theme::text_muted())
                                        .child(dir),
                                ),
                        )
                        .child(self.settings_button(
                            "plugins-open-folder",
                            "Open",
                            cx,
                            |this, cx| {
                                if let Err(error) = crate::ui::plugins::open_plugins_dir() {
                                    this.plugins.error = Some(error);
                                    cx.notify();
                                }
                            },
                        )),
                ),
            )
            .when_some(error, |page, error| {
                page.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::danger())
                        .child(error),
                )
            })
            .children(cards)
            .when(list.is_empty(), |page| {
                page.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::text_muted())
                        .child("No plugins found."),
                )
            })
            .into_any_element()
    }

    fn plugin_card(&self, plugin: &PluginInfo, cx: &mut Context<Self>) -> AnyElement {
        let runtime = match plugin.runtime {
            PluginRuntime::Native => "native",
            PluginRuntime::Js => "js",
        };
        let mut meta = plugin.id.clone();
        if !plugin.version.is_empty() {
            meta.push_str(" · ");
            meta.push_str(&plugin.version);
        }
        if !plugin.author.is_empty() {
            meta.push_str(" · ");
            meta.push_str(&plugin.author);
        }
        meta.push_str(" · ");
        meta.push_str(runtime);

        let settings: Vec<AnyElement> = plugin
            .settings
            .iter()
            .map(|spec| self.plugin_setting(plugin, spec, cx))
            .collect();
        let urls: Vec<AnyElement> = plugin
            .http
            .urls
            .iter()
            .enumerate()
            .map(|(index, url)| self.plugin_url(&plugin.id, index, url, cx))
            .collect();

        super::card()
            .py(px(8.))
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.))
                            .gap(px(2.))
                            .child(div().text_size(px(13.)).child(plugin.name.clone()))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme::text_muted())
                                    .child(meta),
                            )
                            .when(!plugin.description.is_empty(), |col| {
                                let description = plugin.description.clone();
                                col.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(theme::text_muted())
                                        .child(description),
                                )
                            })
                            .when_some(plugin.error.clone(), |col, error| {
                                col.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(theme::danger())
                                        .child(error),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .flex_shrink_0()
                            .children(http_badge(&plugin.http))
                            .child(self.plugin_switch(&plugin.id, plugin.enabled, cx)),
                    ),
            )
            .children(settings)
            .when(!urls.is_empty(), |card| {
                card.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().text_size(px(13.)).child("URL"))
                        .children(urls),
                )
            })
            .when_some(plugin.http.last_error.clone(), |card, error| {
                card.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::danger())
                        .child(error),
                )
            })
            .when(!plugin.log.is_empty(), |card| {
                let lines = plugin.log.clone();
                card.child(
                    div().flex().flex_col().gap(px(2.)).children(
                        lines.into_iter().map(|line| {
                            div()
                                .text_size(px(11.))
                                .text_color(theme::text_muted())
                                .child(line)
                                .into_any_element()
                        }),
                    ),
                )
            })
            .into_any_element()
    }

    fn plugin_setting(
        &self,
        plugin: &PluginInfo,
        spec: &PluginSettingSpec,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if spec.label.is_empty() {
            spec.key.clone()
        } else {
            spec.label.clone()
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .py(px(4.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w(px(0.))
                    .gap(px(2.))
                    .child(div().text_size(px(13.)).child(label))
                    .when(!spec.description.is_empty(), |col| {
                        let description = spec.description.clone();
                        col.child(
                            div()
                                .text_size(px(12.))
                                .text_color(theme::text_muted())
                                .child(description),
                        )
                    }),
            )
            .child(self.plugin_control(plugin, spec, cx))
            .into_any_element()
    }

    fn plugin_control(
        &self,
        plugin: &PluginInfo,
        spec: &PluginSettingSpec,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match spec.kind {
            SettingKind::Boolean => {
                let on = plugin
                    .config
                    .get(&spec.key)
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.plugin_bool_switch(&plugin.id, &spec.key, on, cx)
            }
            SettingKind::Button => self.plugin_button(&plugin.id, spec, cx),
            SettingKind::Slider => self.slider_control(plugin, spec, cx),
            SettingKind::Number => self.number_control(plugin, spec, cx),
            SettingKind::String => self.text_control(plugin, spec, cx),
        }
    }

    fn text_control(
        &self,
        plugin: &PluginInfo,
        spec: &PluginSettingSpec,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = draft_key(&plugin.id, &spec.key);
        let Some(field) = self.plugins.fields.get(&key).cloned() else {
            return div().into_any_element();
        };
        div()
            .w(px(160.))
            .h(px(28.))
            .px(px(8.))
            .flex_shrink_0()
            .rounded_md()
            .bg(theme::bg_elevated())
            .child(field)
            .into_any_element()
    }

    fn number_control(
        &self,
        plugin: &PluginInfo,
        spec: &PluginSettingSpec,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = draft_key(&plugin.id, &spec.key);
        let Some(field) = self.plugins.fields.get(&key).cloned() else {
            return div().into_any_element();
        };
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .flex_shrink_0()
            .child(self.step_button(&plugin.id, spec, -1.0, "−", cx))
            .child(
                div()
                    .w(px(88.))
                    .h(px(28.))
                    .px(px(8.))
                    .rounded_md()
                    .bg(theme::bg_elevated())
                    .child(field),
            )
            .child(self.step_button(&plugin.id, spec, 1.0, "+", cx))
            .into_any_element()
    }

    fn slider_control(
        &self,
        plugin: &PluginInfo,
        spec: &PluginSettingSpec,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let min = spec.min.unwrap_or(0.0);
        let max = spec.max.unwrap_or(100.0);
        let span = (max - min).abs().max(f64::EPSILON);
        let value = plugin
            .config
            .get(&spec.key)
            .and_then(Value::as_f64)
            .unwrap_or(min);
        let frac = ((value - min) / span).clamp(0.0, 1.0) as f32;
        let bounds_id = format!("plugin-slider:{}:{}", plugin.id, spec.key);
        let paint_id = bounds_id.clone();
        let click_id = bounds_id.clone();
        let plugin_id = plugin.id.clone();
        let spec = spec.clone();
        let drag_plugin = plugin_id.clone();
        let drag_key = spec.key.clone();
        let drag_spec = spec.clone();
        let host = cx.entity().clone();
        let shown = number_text(value);

        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .flex_shrink_0()
            .child(self.step_button(&plugin_id, &spec, -1.0, "−", cx))
            .child(
                div()
                    .id(SharedString::from(bounds_id.clone()))
                    .relative()
                    .w(px(160.))
                    .h(px(22.))
                    .cursor_pointer()
                    .child(
                        canvas(
                            move |bounds, _window, app| {
                                let paint_id = paint_id.clone();
                                host.update(app, |view, _cx| {
                                    view.plugins.bounds.insert(paint_id, bounds);
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .right(px(0.))
                            .top(px(9.))
                            .h(px(4.))
                            .rounded_full()
                            .bg(theme::bg_elevated())
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(frac))
                                    .rounded_full()
                                    .bg(theme::accent()),
                            ),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(6.))
                            .left(relative(frac))
                            .ml(px(-5.))
                            .size(px(10.))
                            .rounded_full()
                            .bg(theme::white()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, _, cx| {
                            let Some(bounds) = this.plugins.bounds.get(&click_id).copied() else {
                                return;
                            };
                            let width = bounds.size.width.as_f32().max(1.0);
                            let frac = ((event.position.x - bounds.origin.x).as_f32() / width)
                                .clamp(0.0, 1.0);
                            let next = min + (max - min) * (frac as f64);
                            let value = drag_spec.sanitize(Some(&json!(next)));
                            this.write_plugin_setting(&drag_plugin, &drag_key, value, cx);
                        }),
                    ),
            )
            .child(self.step_button(&plugin_id, &spec, 1.0, "+", cx))
            .child(
                div()
                    .min_w(px(28.))
                    .text_size(px(12.))
                    .text_color(theme::text_muted())
                    .child(shown),
            )
            .into_any_element()
    }

    fn step_button(
        &self,
        plugin_id: &str,
        spec: &PluginSettingSpec,
        delta: f64,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = SharedString::from(format!(
            "plugin-step:{plugin_id}:{}:{delta}",
            spec.key
        ));
        let arm_id = id.clone();
        let release_id = id.clone();
        let plugin_id = plugin_id.to_string();
        let spec = spec.clone();
        div()
            .id(id)
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(theme::bg_elevated())
            .cursor_pointer()
            .text_size(px(14.))
            .hover(|style| style.bg(theme::hover()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                    this.arm(arm_id.clone());
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.armed.as_ref() != Some(&release_id) {
                        return;
                    }
                    let current = this
                        .plugins
                        .list
                        .iter()
                        .find(|plugin| plugin.id == plugin_id)
                        .and_then(|plugin| plugin.config.get(&spec.key))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    let step = spec.step.filter(|step| *step > 0.0).unwrap_or(1.0);
                    let value = spec.sanitize(Some(&json!(current + delta * step)));
                    this.write_plugin_setting(&plugin_id, &spec.key, value, cx);
                }),
            )
            .child(label)
            .into_any_element()
    }

    fn plugin_button(
        &self,
        plugin_id: &str,
        spec: &PluginSettingSpec,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = SharedString::from(format!("plugin-btn:{plugin_id}:{}", spec.key));
        let arm_id = id.clone();
        let release_id = id.clone();
        let plugin_id = plugin_id.to_string();
        let key = spec.key.clone();
        let label = if spec.label.is_empty() {
            spec.key.clone()
        } else {
            spec.label.clone()
        };
        div()
            .id(id)
            .h(px(28.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded_md()
            .bg(theme::bg_elevated())
            .cursor_pointer()
            .text_size(px(12.))
            .hover(|style| style.bg(theme::hover()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                    this.arm(arm_id.clone());
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.armed.as_ref() != Some(&release_id) {
                        return;
                    }
                    this.write_plugin_setting(&plugin_id, &key, json!(true), cx);
                }),
            )
            .child(label)
            .into_any_element()
    }

    fn plugin_switch(&self, plugin_id: &str, on: bool, cx: &mut Context<Self>) -> AnyElement {
        let id = SharedString::from(format!("plugin-on:{plugin_id}"));
        let arm_id = id.clone();
        let release_id = id.clone();
        let plugin_id = plugin_id.to_string();
        div()
            .id(id)
            .w(px(36.))
            .h(px(20.))
            .rounded_full()
            .p(px(2.))
            .cursor_pointer()
            .bg(if on {
                theme::accent()
            } else {
                theme::bg_elevated()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                    this.arm(arm_id.clone());
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.armed.as_ref() != Some(&release_id) {
                        return;
                    }
                    this.set_plugin_enabled(&plugin_id, !on, cx);
                }),
            )
            .child(
                div()
                    .size(px(16.))
                    .rounded_full()
                    .bg(theme::white())
                    .when(on, |knob| knob.ml(px(16.))),
            )
            .into_any_element()
    }

    fn plugin_bool_switch(
        &self,
        plugin_id: &str,
        key: &str,
        on: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = SharedString::from(format!("plugin-bool:{plugin_id}:{key}"));
        let arm_id = id.clone();
        let release_id = id.clone();
        let plugin_id = plugin_id.to_string();
        let key = key.to_string();
        div()
            .id(id)
            .w(px(36.))
            .h(px(20.))
            .rounded_full()
            .p(px(2.))
            .cursor_pointer()
            .bg(if on {
                theme::accent()
            } else {
                theme::bg_elevated()
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                    this.arm(arm_id.clone());
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.armed.as_ref() != Some(&release_id) {
                        return;
                    }
                    this.write_plugin_setting(&plugin_id, &key, json!(!on), cx);
                }),
            )
            .child(
                div()
                    .size(px(16.))
                    .rounded_full()
                    .bg(theme::white())
                    .when(on, |knob| knob.ml(px(16.))),
            )
            .into_any_element()
    }

    fn plugin_url(
        &self,
        plugin_id: &str,
        index: usize,
        url: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = SharedString::from(format!("plugin-url:{plugin_id}:{index}"));
        let arm_id = id.clone();
        let release_id = id.clone();
        let url = url.to_string();
        let shown = url.clone();
        div()
            .id(id)
            .text_size(px(12.))
            .text_color(theme::accent())
            .cursor_pointer()
            .child(shown)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseDownEvent, _, _cx| {
                    this.arm(arm_id.clone());
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &gpui::MouseUpEvent, _, cx| {
                    if this.armed.as_ref() != Some(&release_id) {
                        return;
                    }
                    if let Err(error) = crate::ui::plugins::open_url(&url) {
                        this.plugins.error = Some(error);
                        cx.notify();
                    }
                }),
            )
            .into_any_element()
    }

    fn set_plugin_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        match crate::ui::plugins::set_enabled(cx, id, enabled) {
            Ok(_) => {
                self.plugins.error = None;
                self.reload_plugins(cx);
            }
            Err(error) => {
                self.plugins.error = Some(error);
                cx.notify();
            }
        }
    }

    fn write_plugin_setting(
        &mut self,
        plugin_id: &str,
        key: &str,
        value: Value,
        cx: &mut Context<Self>,
    ) {
        let draft = draft_key(plugin_id, key);
        let display = json_display(&value);
        self.plugins.synced.insert(draft.clone(), display.clone());
        if let Some(field) = self.plugins.fields.get(&draft).cloned() {
            // Enter is handled inside the field. Updating it here would borrow it twice.
            cx.defer(move |app| {
                field.update(app, |field, cx| {
                    if field.content.to_string() != display {
                        field.replace_text(display, cx);
                    }
                });
            });
        }
        match crate::ui::plugins::set_config(cx, plugin_id, json!({ key: value })) {
            Ok(_) => {
                self.plugins.error = None;
                self.reload_plugins(cx);
            }
            Err(error) => {
                self.plugins.error = Some(error);
                cx.notify();
            }
        }
    }
}

fn make_plugin_field(
    draft_key: &str,
    initial: &str,
    cx: &mut Context<SettingsWindow>,
) -> gpui::Entity<TextField> {
    let draft_key = draft_key.to_string();
    let initial = initial.to_string();
    let settings = cx.entity().clone();
    cx.new(move |cx| {
        let element_id = SharedString::from(format!("plugin-field:{draft_key}"));
        let submit_key = draft_key.clone();
        let mut field = TextField::new(cx, "", |_, _, _| {}, |_, _| {}).element_id(element_id);
        field.set_on_submit(move |text, _window, app| {
            let text = text.to_string();
            let submit_key = submit_key.clone();
            settings.update(app, |this, cx| {
                this.commit_plugin_text(&submit_key, &text, cx);
            });
        });
        field.replace_text(initial, cx);
        field
    })
}

impl SettingsWindow {
    fn commit_plugin_text(&mut self, draft_key: &str, text: &str, cx: &mut Context<Self>) {
        let Some((plugin_id, key)) = draft_key.split_once(':') else {
            return;
        };
        let Some(spec) = self
            .plugins
            .list
            .iter()
            .find(|plugin| plugin.id == plugin_id)
            .and_then(|plugin| plugin.settings.iter().find(|spec| spec.key == key))
            .cloned()
        else {
            return;
        };
        let value = match spec.kind {
            SettingKind::Number | SettingKind::Slider => {
                let Ok(parsed) = text.trim().parse::<f64>() else {
                    self.plugins.error = Some("Enter a number".into());
                    cx.notify();
                    return;
                };
                spec.sanitize(Some(&json!(parsed)))
            }
            SettingKind::String => json!(text),
            _ => return,
        };
        self.write_plugin_setting(plugin_id, key, value, cx);
    }
}

fn draft_key(plugin_id: &str, key: &str) -> String {
    format!("{plugin_id}:{key}")
}

fn setting_text(config: &Value, spec: &PluginSettingSpec) -> String {
    match spec.kind {
        SettingKind::Number | SettingKind::Slider => config
            .get(&spec.key)
            .and_then(Value::as_f64)
            .map(number_text)
            .unwrap_or_else(|| "0".into()),
        _ => config
            .get(&spec.key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }
}

fn number_text(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn json_display(value: &Value) -> String {
    match value {
        Value::Number(number) => number
            .as_f64()
            .map(number_text)
            .unwrap_or_else(|| number.to_string()),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn http_badge(status: &crate::ui::plugins::HttpStatus) -> Option<AnyElement> {
    let (text, color) = if status.running {
        ("Running", theme::accent())
    } else if status.last_error.is_some() {
        ("Error", theme::danger())
    } else if status.enabled {
        ("Starting…", theme::text_muted())
    } else {
        return None;
    };
    Some(
        div()
            .px(px(8.))
            .h(px(20.))
            .flex()
            .items_center()
            .rounded_full()
            .bg(theme::bg_elevated())
            .text_size(px(11.))
            .text_color(color)
            .child(text)
            .into_any_element(),
    )
}
