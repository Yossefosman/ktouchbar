// SPDX-License-Identifier: GPL-3.0-only
use cairo::Context;
use input_linux::Key;
use serde::de;
use serde::Deserialize;

pub mod battery;
pub mod content;
pub mod dynamicshortcuts;
pub mod icon;
pub mod layout;
pub mod slider;

pub use battery::{BatteryInfo, BatteryMonitor, BatteryState};
pub use content::{BatteryIconMode, BatteryImages, WidgetContent};
pub use icon::ICON_LOADER;
pub use slider::{new_slider_state, SliderColors, SliderState};

pub use crate::config::SliderType;

use crate::config::{OutlineColor, PaddingSpec, WidgetConfig};

// ── Action enum ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Key(Vec<Key>),
    Panel(String),
    Back,
    Exec(String),
}

impl<'de> Deserialize<'de> for Action {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ActionProxy {
            Key { key: Vec<Key> },
            Panel { panel: String },
            Back { back: bool },
            Exec { exec: String },
        }
        match ActionProxy::deserialize(deserializer)? {
            ActionProxy::Key { key } => Ok(Action::Key(key)),
            ActionProxy::Panel { panel } => Ok(Action::Panel(panel)),
            ActionProxy::Back { back: true } => Ok(Action::Back),
            ActionProxy::Back { back: false } => {
                Err(de::Error::invalid_value(de::Unexpected::Bool(false), &"back = true"))
            }
            ActionProxy::Exec { exec } => Ok(Action::Exec(exec)),
        }
    }
}

// ── Widget types ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ContainerState {
    pub children: Vec<Widget>,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum WidgetKind {
    Content(WidgetContent),
    Slider(SliderState),
    Container(ContainerState),
}

#[derive(Clone, Debug)]
pub struct Widget {
    pub kind: WidgetKind,
    pub changed: bool,
    pub active: bool,
    pub action: Option<Action>,
    pub show_outline: Option<bool>,
    pub outline_color: Option<OutlineColor>,
    pub active_color: Option<OutlineColor>,
    pub padding_left: Option<u16>,
    pub padding_right: Option<u16>,
    pub margin_left: u16,
    pub margin_right: u16,
    pub font_size: Option<f64>,
    pub icon_size: Option<f64>,
    pub min_width: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct WidgetConfigCtx<'a> {
    pub global_theme: Option<&'a str>,
    pub default_padding: Option<u16>,
    pub slider_color_source: Option<&'a str>,
    pub global_outline_color: Option<&'a OutlineColor>,
    pub global_active_color: Option<&'a OutlineColor>,
    pub global_slider_live_update: Option<bool>,
    pub global_track_outline: Option<&'a OutlineColor>,
}

// ── FunctionLayer ────────────────────────────────────────────────

#[derive(Default, Clone)]
pub struct FunctionLayer {
    pub displays_time: bool,
    pub displays_battery: bool,
    pub widgets: Vec<(usize, Widget)>,
}

// ── Widget impl ──────────────────────────────────────────────────

fn resolve_padding(p: Option<PaddingSpec>, default_padding: Option<u16>) -> Option<u16> {
    match p {
        None => None,
        Some(PaddingSpec::Value(v)) => Some(v),
        Some(PaddingSpec::Default) => default_padding,
    }
}

impl Widget {
    pub fn is_slider(&self) -> bool {
        matches!(self.kind, WidgetKind::Slider { .. })
    }

    pub fn is_battery(&self) -> bool {
        match &self.kind {
            WidgetKind::Content(WidgetContent::Battery { .. }) => true,
            WidgetKind::Container(state) => state.children.iter().any(|c| c.is_battery()),
            _ => false,
        }
    }

    pub fn dragging(&self) -> bool {
        match &self.kind {
            WidgetKind::Slider(s) => s.dragging,
            WidgetKind::Container(state) => state.children.iter().any(|c| c.dragging()),
            _ => false,
        }
    }

    pub fn set_dragging(&mut self, val: bool) {
        match &mut self.kind {
            WidgetKind::Slider(ref mut s) => s.dragging = val,
            WidgetKind::Container(ref mut state) => {
                for child in &mut state.children {
                    child.set_dragging(val);
                }
            }
            _ => {}
        }
    }

    pub fn live_update(&self) -> bool {
        match &self.kind {
            WidgetKind::Slider(s) => s.live_update,
            WidgetKind::Container(state) => state.children.iter().any(|c| c.live_update()),
            _ => false,
        }
    }

    pub fn with_config(cfg: WidgetConfig, ctx: &WidgetConfigCtx) -> Widget {
        let mut common = match &cfg {
            WidgetConfig::Button { common, .. } => common.clone(),
            WidgetConfig::Slider { common, .. } => common.clone(),
            WidgetConfig::Battery { common, .. } => common.clone(),
            WidgetConfig::Clock { common, .. } => common.clone(),
            WidgetConfig::Dynamic { common, .. } => common.clone(),
            WidgetConfig::Container { common, .. } => common.clone(),
        };
        let theme = common.theme.take().or_else(|| ctx.global_theme.map(String::from));

        let pad = resolve_padding(common.padding, ctx.default_padding);
        let padding_left = resolve_padding(common.padding_left, ctx.default_padding).or(pad);
        let padding_right = resolve_padding(common.padding_right, ctx.default_padding).or(pad);

        let margin = common.margin.unwrap_or(0);
        let margin_left = common.margin_left.unwrap_or(margin);
        let margin_right = common.margin_right.unwrap_or(margin);

        let mut widget = match cfg {
            WidgetConfig::Dynamic { .. } => {
                let window_class = match dynamicshortcuts::get_active_window_info() {
                    Ok(info) => {
                        if crate::config::DEBUG_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
                            println!("KDE active window: class='{}'", info.class);
                        }
                        info.class
                    }
                    Err(e) => {
                        if crate::config::DEBUG_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
                            println!("KDE window tracker error: {}", e);
                        }
                        String::new()
                    }
                };
                Widget {
                    kind: WidgetKind::Content(WidgetContent::Text { text: window_class }),
                    action: None,
                    active: false, changed: false,
                    show_outline: None, outline_color: None, active_color: None,
                    padding_left, padding_right, margin_left, margin_right,
                    font_size: None, icon_size: None, min_width: 0.0,
                }
            }
            WidgetConfig::Button { icon, text, action, .. } => {
                if let Some(t) = text {
                    Widget {
                        kind: WidgetKind::Content(WidgetContent::Text { text: t }),
                        action,
                        active: false, changed: false,
                        show_outline: None, outline_color: None, active_color: None,
                        padding_left, padding_right, margin_left, margin_right,
                        font_size: None, icon_size: None, min_width: 0.0,
                    }
                } else if let Some(icon_name) = icon {
                    let content = WidgetContent::new_icon(icon_name, theme.as_ref());
                    Widget {
                        kind: WidgetKind::Content(content),
                        action,
                        active: false, changed: false,
                        show_outline: None, outline_color: None, active_color: None,
                        padding_left, padding_right, margin_left, margin_right,
                        font_size: None, icon_size: None, min_width: 0.0,
                    }
                } else {
                    panic!("Button widget must have icon or text")
                }
            }
            WidgetConfig::Slider { slider, icon, text, handle_icon, bar_icon, handle_size, live_update, .. } => {
                let hs = handle_size.unwrap_or(20) as f64;
                let colors = slider::load_slider_colors(ctx.slider_color_source.unwrap_or("kde"));
                let label_icon = icon.as_ref().and_then(|i| {
                    let wc = WidgetContent::new_icon(i, theme.as_ref());
                    match wc {
                        WidgetContent::Text { .. } => None,
                        other => Some(other),
                    }
                });
                let lu = live_update.or(ctx.global_slider_live_update).unwrap_or(false);
                let track_outline = common.slider_track_outline.or_else(|| ctx.global_track_outline.cloned());
                let state = new_slider_state(slider, handle_icon.as_deref(), bar_icon.as_deref(), hs, label_icon, text, colors, lu, track_outline);
                Widget {
                    kind: WidgetKind::Slider(state),
                    action: None,
                    active: false, changed: false,
                    show_outline: None, outline_color: None, active_color: None,
                    padding_left, padding_right, margin_left, margin_right,
                    font_size: None, icon_size: None, min_width: 0.0,
                }
            }
            WidgetConfig::Battery { mode, .. } => {
                if let Some(battery) = battery::find_battery_device() {
                    let content = WidgetContent::new_battery(battery, mode, theme.as_ref());
                    Widget {
                        kind: WidgetKind::Content(content),
                        action: None,
                        active: false, changed: false,
                        show_outline: None, outline_color: None, active_color: None,
                        padding_left, padding_right, margin_left, margin_right,
                        font_size: None, icon_size: None, min_width: 0.0,
                    }
                } else {
                    Widget {
                        kind: WidgetKind::Content(WidgetContent::Text { text: "Battery N/A".to_string() }),
                        action: None,
                        active: false, changed: false,
                        show_outline: None, outline_color: None, active_color: None,
                        padding_left, padding_right, margin_left, margin_right,
                        font_size: None, icon_size: None, min_width: 0.0,
                    }
                }
            }
            WidgetConfig::Clock { format, locale, .. } => {
                let content = WidgetContent::new_time(&format, locale.as_deref());
                Widget {
                    kind: WidgetKind::Content(content),
                    action: None,
                    active: false, changed: false,
                    show_outline: None, outline_color: None, active_color: None,
                    padding_left, padding_right, margin_left, margin_right,
                    font_size: None, icon_size: None, min_width: 0.0,
                }
            }
            WidgetConfig::Container { widgets, .. } => {
                let container_theme = common.theme.as_deref().or(ctx.global_theme);
                let child_ctx = WidgetConfigCtx {
                    global_theme: container_theme,
                    ..*ctx
                };
                let mut children: Vec<Widget> = widgets.into_iter().map(|wc| {
                    Widget::with_config(wc, &child_ctx)
                }).collect();
                for child in &mut children {
                    child.font_size = child.font_size.or(common.font_size);
                    child.icon_size = child.icon_size.or(common.icon_size);
                }
                Widget {
                    kind: WidgetKind::Container(ContainerState { children }),
                    action: None,
                    active: false, changed: false,
                    show_outline: None, outline_color: None, active_color: None,
                    padding_left, padding_right, margin_left, margin_right,
                    font_size: None, icon_size: None, min_width: 0.0,
                }
            }
        };

        widget.show_outline = common.show_outline;
        widget.outline_color = common.outline_color.or(ctx.global_outline_color.cloned());
        widget.active_color = common.active_color.or(ctx.global_active_color.cloned());
        widget.font_size = common.font_size;
        widget.icon_size = common.icon_size;
        widget
    }

    pub fn update_slider_value(&mut self, x: f64, widget_x: f64, widget_width: f64) {
        if let WidgetKind::Slider(ref mut state) = &mut self.kind {
            let slider_start = widget_x + state.label_w;
            let slider_w = widget_width - state.label_w;
            if slider_w > 0.0 {
                let new_val = ((x - slider_start) / slider_w).clamp(0.0, 1.0);
                state.value = new_val;
                self.changed = true;
            }
        }
    }

    pub fn commit_slider_value(&mut self) {
        self.write_slider_value();
        if let WidgetKind::Slider(ref mut state) = &mut self.kind {
            state.dragging = false;
        }
    }

    pub fn write_slider_value(&mut self) {
        if let WidgetKind::Slider(ref mut state) = &mut self.kind {
            slider::write_slider_value(state.slider_type, state.value);
            let (_val, _text) = slider::read_slider_value(state.slider_type);
            state.value = _val;
            self.changed = true;
        }
    }

    pub fn set_background_color(&self, c: &Context, color: f64) {
        if let WidgetKind::Content(ref wc) = self.kind {
            wc.set_background_color(c, color);
        } else {
            c.set_source_rgb(color, color, color);
        }
    }

    pub fn measure_min_width(&mut self, c: &Context, icon_size: f64) {
        self.min_width = self.kind.measure_min_width(c, icon_size);
    }
}

// ── FunctionLayer impl ───────────────────────────────────────────

impl FunctionLayer {
    pub fn with_config(widgets_cfg: Vec<WidgetConfig>, ctx: &WidgetConfigCtx) -> FunctionLayer {
        if widgets_cfg.is_empty() {
            panic!("Invalid configuration, layer has 0 widgets");
        }
        let mut displays_time = false;
        let mut displays_battery = false;
        let widgets: Vec<(usize, Widget)> = widgets_cfg
            .into_iter()
            .enumerate()
            .map(|(i, w)| {
                displays_time |= matches!(w, WidgetConfig::Clock { .. });
                displays_battery |= matches!(w, WidgetConfig::Battery { .. });
                let widget = Widget::with_config(w, ctx);
                (i, widget)
            })
            .collect();
        FunctionLayer { displays_time, displays_battery, widgets }
    }
}
