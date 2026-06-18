// SPDX-License-Identifier: GPL-3.0-only
use anyhow::{anyhow, Context as _, Result};
use cairo::{Antialias, Context, FontSlant, FontWeight, Format, ImageSurface};
use chrono::{
    format::{Item as ChronoItem, Locale, StrftimeItems},
    Local,
};
use crate::icon_cache::get_icon_flexible;
use librsvg_rebind::{prelude::HandleExt, Handle, Rectangle};
use std::path::Path;
use std::fs::{self, File};
use std::sync::atomic::Ordering;

use crate::battery_monitor::BatteryState;
use crate::icon_cache;
use crate::config::{Action, OutlineColor, WidgetConfig, Config, PaddingSpec};
use crate::dynamicshortcuts;
use crate::slider::{self, SliderType, SliderColors};
use cairo::Surface;
use drm::control::ClipRect;

#[derive(Debug, Clone)]
pub enum WidgetContent {
    Text {
        text: String,
    },
    Svg {
        handle: Handle,
    },
    Bitmap {
        surface: ImageSurface,
    },
    Time {
        format: Vec<ChronoItem<'static>>,
        locale: Locale,
    },
    Battery {
        battery: String,
        mode: BatteryIconMode,
        images: BatteryImages,
    },
}

#[derive(Debug, Clone)]
pub struct SliderState {
    pub slider_type: SliderType,
    pub value: f64,
    pub handle_surface: Option<ImageSurface>,
    pub bar_surface: Option<ImageSurface>,
    pub handle_size: f64,
    pub colors: SliderColors,
    pub label_icon: Option<WidgetContent>,
    pub label_text: Option<String>,
    pub dragging: bool,
    pub label_w: f64,
    pub live_update: bool,
    pub track_outline: Option<OutlineColor>,
}

#[derive(Debug, Clone)]
pub struct ContainerState {
    pub children: Vec<Widget>,
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

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum WidgetKind {
    Content(WidgetContent),
    Slider(SliderState),
    Container(ContainerState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryIconMode {
    Percentage,
    Icon,
    Both,
}

impl BatteryIconMode {
    pub fn should_draw_icon(self) -> bool {
        self != BatteryIconMode::Percentage
    }
    pub fn should_draw_text(self) -> bool {
        self != BatteryIconMode::Icon
    }
}

#[derive(Clone, Debug)]
pub struct BatteryImages {
    pub plain: Vec<Handle>,
    pub charging: Vec<Handle>,
    pub bolt: Handle,
}

fn make_placeholder_icon(name: &str) -> WidgetContent {
    let surface = ImageSurface::create(Format::ARgb32, ICON_SIZE, ICON_SIZE).unwrap();
    let c = Context::new(&surface).unwrap();
    let s = ICON_SIZE as f64;

    c.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    c.paint().unwrap();

    let inset = 3.0;
    c.set_source_rgb(0.35, 0.35, 0.35);
    c.set_line_width(1.5);
    c.rectangle(inset, inset, s - inset * 2.0, s - inset * 2.0);
    c.stroke().unwrap();

    c.set_source_rgb(0.55, 0.55, 0.55);
    c.select_font_face("sans-serif", FontSlant::Normal, FontWeight::Normal);
    c.set_font_size(14.0);

    let label = if name.is_empty() { "?" } else { name };
    let display = if label.len() > 4 { &label[..4] } else { label };
    let extents = c.text_extents(display).unwrap();
    c.move_to(
        (s - extents.width()) / 2.0 - extents.x_bearing(),
        (s - extents.height()) / 2.0 - extents.y_bearing(),
    );
    c.show_text(display).unwrap();

    WidgetContent::Bitmap { surface }
}

#[allow(clippy::too_many_arguments)]
pub fn new_slider_state(
    slider_type: SliderType,
    handle_icon: Option<&str>,
    bar_icon: Option<&str>,
    handle_size: f64,
    label_icon: Option<WidgetContent>,
    label_text: Option<String>,
    colors: SliderColors,
    live_update: bool,
    track_outline: Option<OutlineColor>,
) -> SliderState {
    let (value, _display_text) = slider::read_slider_value(slider_type);
    let handle_surface = handle_icon.and_then(|s| slider::load_slider_icon(s, handle_size));
    let bar_surface = bar_icon.and_then(|s| {
        slider::load_slider_icon(s, 48.0)
    });
    SliderState {
        slider_type, value, handle_surface, bar_surface, handle_size,
        colors, label_icon, label_text, dragging: false, label_w: 0.0, live_update,
        track_outline,
    }
}

impl WidgetContent {
    pub fn new_icon(name: impl AsRef<str>, theme: Option<impl AsRef<str>>) -> WidgetContent {
        try_load_image(name.as_ref(), theme).unwrap_or_else(|_| {
            make_placeholder_icon(name.as_ref())
        })
    }

    pub fn new_time(format: &str, locale_str: Option<&str>) -> WidgetContent {
        let format_str = if format == "24hr" {
            "%H:%M    %a %-e %b"
        } else if format == "12hr" {
            "%-l:%M %p    %a %-e %b"
        } else {
            format
        };

        let format_items = match StrftimeItems::new(format_str).parse_to_owned() {
            Ok(s) => s,
            Err(e) => panic!("Invalid time format: {e:?}"),
        };

        let locale = locale_str
            .and_then(|l| Locale::try_from(l).ok())
            .unwrap_or(Locale::POSIX);

        WidgetContent::Time { format: format_items, locale }
    }

    pub fn new_battery(
        battery: String,
        battery_mode: String,
        theme: Option<impl AsRef<str>>,
    ) -> WidgetContent {
        let bolt = load_battery_image("battery-100-charging", theme.as_ref());
        let mut plain = Vec::new();
        let mut charging = Vec::new();
        for icon in [
            "battery-000", "battery-020", "battery-030", "battery-050",
            "battery-060", "battery-080", "battery-090", "battery-100",
        ] {
            plain.push(load_battery_image(icon, theme.as_ref()));
        }
        for icon in [
            "battery-020-charging", "battery-030-charging", "battery-050-charging",
            "battery-060-charging", "battery-080-charging", "battery-090-charging",
            "battery-100-charging",
        ] {
            charging.push(load_battery_image(icon, theme.as_ref()));
        }
        let mode = match battery_mode.as_str() {
            "icon" => BatteryIconMode::Icon,
            "percentage" => BatteryIconMode::Percentage,
            "both" => BatteryIconMode::Both,
            _ => panic!("invalid battery mode, accepted: icon, percentage, both"),
        };
        WidgetContent::Battery { battery, mode, images: BatteryImages { plain, bolt, charging } }
    }

    pub fn icon_from_handle(handle: Handle) -> WidgetContent {
        WidgetContent::Svg { handle }
    }

    pub fn icon_from_surface(surface: ImageSurface) -> WidgetContent {
        WidgetContent::Bitmap { surface }
    }

    pub fn render_at(&self, c: &Context, x: f64, width: f64, height: f64, icon_size: f64) {
        let font_extents = c.font_extents().unwrap();
        let baseline_y = height / 2.0 + (font_extents.ascent() - font_extents.descent()) / 2.0;
        match self {
            WidgetContent::Text { text } => {
                let extents = c.text_extents(text).unwrap();
                c.move_to(
                    (x + width / 2.0 - extents.width() / 2.0).round(),
                    baseline_y.round(),
                );
                c.show_text(text).unwrap();
            }
            WidgetContent::Svg { handle } => {
                let icon_x = (x + width / 2.0 - icon_size / 2.0).round();
                let icon_y = ((height - icon_size) / 2.0).round();
                handle.render_document(c, &Rectangle::new(icon_x, icon_y, icon_size, icon_size)).unwrap();
            }
            WidgetContent::Bitmap { surface } => {
                let icon_x = (x + width / 2.0 - icon_size / 2.0).round();
                let icon_y = ((height - icon_size) / 2.0).round();
                c.set_source_surface(surface, icon_x, icon_y).unwrap();
                c.rectangle(icon_x, icon_y, icon_size, icon_size);
                c.fill().unwrap();
            }
            WidgetContent::Time { format, locale } => {
                let current_time = Local::now();
                let formatted_time = current_time
                    .format_localized_with_items(format.iter(), *locale)
                    .to_string();
                let extents = c.text_extents(&formatted_time).unwrap();
                c.move_to(
                    (x + width / 2.0 - extents.width() / 2.0).round(),
                    baseline_y.round(),
                );
                c.show_text(&formatted_time).unwrap();
            }
            WidgetContent::Battery { battery, mode, images } => {
                let (capacity, state) = get_battery_state(battery);
                let icon = if mode.should_draw_icon() {
                    Some(match state {
                        BatteryState::Charging => match capacity {
                            0..=20 => &images.charging[0],
                            21..=30 => &images.charging[1],
                            31..=50 => &images.charging[2],
                            51..=60 => &images.charging[3],
                            61..=80 => &images.charging[4],
                            81..=99 => &images.charging[5],
                            _ => &images.charging[6],
                        },
                        _ => match capacity {
                            0 => &images.plain[0],
                            1..=20 => &images.plain[1],
                            21..=30 => &images.plain[2],
                            31..=50 => &images.plain[3],
                            51..=60 => &images.plain[4],
                            61..=80 => &images.plain[5],
                            81..=99 => &images.plain[6],
                            _ => &images.plain[7],
                        },
                    })
                } else if state == BatteryState::Charging {
                    Some(&images.bolt)
                } else {
                    None
                };
                let percent_str = format!("{:.0}%", capacity);
                let extents = c.text_extents(&percent_str).unwrap();
                let mut total_width = extents.width();
                let mut text_offset = 0.0;
                if let Some(svg) = icon {
                    if !mode.should_draw_text() {
                        total_width = icon_size;
                    } else {
                        total_width += icon_size;
                    }
                    text_offset = icon_size;
                    let icon_x = (x + width / 2.0 - total_width / 2.0).round();
                    let icon_y = ((height - icon_size) / 2.0).round();
                    svg.render_document(c, &Rectangle::new(icon_x, icon_y, icon_size, icon_size)).unwrap();
                }
                if mode.should_draw_text() {
                    c.move_to(
                        (x + width / 2.0 - total_width / 2.0 + text_offset).round(),
                        baseline_y.round(),
                    );
                    c.show_text(&percent_str).unwrap();
                }
            }
        }
    }

    pub fn set_background_color(&self, c: &Context, base_color: f64) {
        match self {
            WidgetContent::Battery { battery, .. } => {
                let (_, state) = get_battery_state(battery);
                match state {
                    BatteryState::NotCharging => c.set_source_rgb(base_color, base_color, base_color),
                    BatteryState::Charging => c.set_source_rgb(0.0, base_color, 0.0),
                    BatteryState::Low => c.set_source_rgb(base_color, 0.0, 0.0),
                }
            }
            _ => c.set_source_rgb(base_color, base_color, base_color),
        }
    }

    pub fn is_battery(&self) -> bool {
        matches!(self, WidgetContent::Battery { .. })
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
}

fn load_battery_image(icon: &str, theme: Option<impl AsRef<str>>) -> Handle {
    if let Ok(WidgetContent::Svg { handle }) = try_load_image(icon, theme) {
        return handle;
    }
    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48">
           <rect x="3" y="3" width="42" height="42" rx="6" fill="none" stroke="#666" stroke-width="2"/>
           <text x="24" y="30" font-family="sans-serif" font-size="14" fill="#888" text-anchor="middle">{}</text>
         </svg>"##,
        if icon.len() > 2 { &icon[..2] } else { icon }
    );
    let tmp = std::env::temp_dir().join(format!("ktouchbar-{icon}.svg"));
    let _ = std::fs::write(&tmp, &svg);
    let handle = Handle::from_file(tmp.to_str().unwrap()).ok().and_then(|h| h).unwrap();
    let _ = std::fs::remove_file(&tmp);
    handle
}

pub fn try_load_svg(path: &str) -> Result<Handle> {
    Handle::from_file(path)?.ok_or(anyhow!("failed to load image"))
}

pub fn try_load_png(path: impl AsRef<Path>) -> Result<ImageSurface> {
    let mut file = File::open(path)?;
    let surf = ImageSurface::create_from_png(&mut file)?;
    if surf.height() == ICON_SIZE && surf.width() == ICON_SIZE {
        return Ok(surf);
    }
    let resized = ImageSurface::create(Format::ARgb32, ICON_SIZE, ICON_SIZE).unwrap();
    let c = Context::new(&resized).unwrap();
    c.scale(
        ICON_SIZE as f64 / surf.width() as f64,
        ICON_SIZE as f64 / surf.height() as f64,
    );
    c.set_source_surface(surf, 0.0, 0.0).unwrap();
    c.set_antialias(Antialias::Best);
    c.paint().unwrap();
    Ok(resized)
}

pub fn try_load_image(
    name: impl AsRef<str>,
    theme: Option<impl AsRef<str>>,
) -> Result<WidgetContent> {
    let name = name.as_ref();
    let theme_str = theme.as_ref().map(|s| s.as_ref());

    if let Some(cached_path) =
        icon_cache::get_icon_cached(name.to_string(), theme_str.map(|s| s.to_string()))
    {
        return match cached_path.extension().and_then(|s| s.to_str()) {
            Some("png") => try_load_png(&cached_path).map(WidgetContent::icon_from_surface),
            Some("svg") => try_load_svg(cached_path.to_str().unwrap_or("")).map(WidgetContent::icon_from_handle),
            _ => Err(anyhow!("Unsupported file format")),
        };
    }

    try_load_image_sync(name, theme_str)
}

fn try_load_image_sync(name: &str, theme: Option<&str>) -> Result<WidgetContent> {
    let icon_path = get_icon_flexible(name, theme);

    match icon_path {
        Some(path) => {
            let result = match path.extension().and_then(|s| s.to_str()) {
                Some("png") => try_load_png(&path).map(WidgetContent::icon_from_surface),
                Some("svg") => try_load_svg(
                    path.to_str().ok_or(anyhow!("image path is not unicode"))?,
                )
                .map(WidgetContent::icon_from_handle),
                _ => Err(anyhow!("invalid file extension")),
            };
            result.context(format!("while loading path {}", path.display()))
        }
        None => Err(anyhow!("icon {name} not found in theme {theme:?}")),
    }
}

pub fn find_battery_device() -> Option<String> {
    let power_supply_path = "/sys/class/power_supply";
    if let Ok(entries) = fs::read_dir(power_supply_path) {
        for entry in entries.flatten() {
            let dev_path = entry.path();
            let type_path = dev_path.join("type");
            if let Ok(typ) = fs::read_to_string(&type_path) {
                if typ.trim() == "Battery" {
                    if let Some(name) = dev_path.file_name().and_then(|n| n.to_str()) {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

pub fn get_battery_state(battery: &str) -> (u32, BatteryState) {
    if let Some(cached_state) = crate::battery_monitor::get_cached_battery_state() {
        return cached_state;
    }
    get_battery_state_direct(battery)
}

fn read_raw_capacity(battery: &str) -> u32 {
    let capacity_path = format!("/sys/class/power_supply/{}/capacity", battery);
    if let Ok(cap) = fs::read_to_string(&capacity_path)
        .and_then(|s| s.trim().parse::<u32>().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
    {
        return cap;
    }

    let charge_now = fs::read_to_string(format!("/sys/class/power_supply/{}/charge_now", battery))
        .ok().and_then(|s| s.trim().parse::<f64>().ok());
    let charge_full = fs::read_to_string(format!("/sys/class/power_supply/{}/charge_full", battery))
        .ok().and_then(|s| s.trim().parse::<f64>().ok());
    if let (Some(now), Some(full)) = (charge_now, charge_full) {
        if full > 0.0 {
            return ((now / full) * 100.0).round() as u32;
        }
    }

    let energy_now = fs::read_to_string(format!("/sys/class/power_supply/{}/energy_now", battery))
        .ok().and_then(|s| s.trim().parse::<f64>().ok());
    let energy_full = fs::read_to_string(format!("/sys/class/power_supply/{}/energy_full", battery))
        .ok().and_then(|s| s.trim().parse::<f64>().ok());
    if let (Some(now), Some(full)) = (energy_now, energy_full) {
        if full > 0.0 {
            return ((now / full) * 100.0).round() as u32;
        }
    }

    100
}

fn get_battery_state_direct(battery: &str) -> (u32, BatteryState) {
    let status_path = format!("/sys/class/power_supply/{}/status", battery);
    let status = fs::read_to_string(&status_path).unwrap_or_else(|_| "Unknown".to_string());

    let capacity = read_raw_capacity(battery);

    let status = match status.trim() {
        "Charging" | "Full" => BatteryState::Charging,
        "Discharging" if capacity < 10 => BatteryState::Low,
        _ => BatteryState::NotCharging,
    };
    (capacity, status)
}

const ICON_SIZE: i32 = 48;

const BUTTON_COLOR_INACTIVE: f64 = 0.200;
const BUTTON_COLOR_ACTIVE: f64 = 0.400;

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

fn resolve_padding(p: Option<PaddingSpec>, default_padding: Option<u16>) -> Option<u16> {
    match p {
        None => None,
        Some(PaddingSpec::Value(v)) => Some(v),
        Some(PaddingSpec::Default) => default_padding,
    }
}

impl Widget {
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
                        if crate::config::DEBUG_ENABLED.load(Ordering::Relaxed) {
                            println!("KDE active window: class='{}'", info.class);
                        }
                        info.class
                    }
                    Err(e) => {
                        if crate::config::DEBUG_ENABLED.load(Ordering::Relaxed) {
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
                if let Some(battery) = find_battery_device() {
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
                // Inherit container's font_size/icon_size as fallback for children
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
            // Sliders use the generic background
            c.set_source_rgb(color, color, color);
        }
    }

    pub fn measure_min_width(&mut self, c: &Context, icon_size: f64) {
        self.min_width = self.kind.measure_min_width(c, icon_size);
    }
}

fn render_slider(state: &SliderState, c: &Context, x: f64, width: f64, height: f64) {
    let cy = height / 2.0;
    let track_w = width * 0.85;
    let track_x = x + (width - track_w) / 2.0;
    let track_h = 6.0f64.max(height * 0.06);
    let track_y = cy - track_h / 2.0;
    let value = state.value;
    let fill_end = track_x + track_w * value;
    let colors = &state.colors;

    // Bar empty portion — full pill from track_x to track_x + track_w
    c.set_source_rgb(colors.bar_empty.0, colors.bar_empty.1, colors.bar_empty.2);
    c.new_sub_path();
    let r = track_h / 2.0;
    c.arc(track_x + track_w - r, track_y + r, r, (-90.0f64).to_radians(), (0.0f64).to_radians());
    c.arc(track_x + track_w - r, track_y + r, r, (0.0f64).to_radians(), (90.0f64).to_radians());
    c.arc(track_x + r, track_y + r, r, (90.0f64).to_radians(), (180.0f64).to_radians());
    c.arc(track_x + r, track_y + r, r, (180.0f64).to_radians(), (270.0f64).to_radians());
    c.close_path();
    c.fill().unwrap();

    // Bar fill portion
    if let Some(ref bar) = state.bar_surface {
        c.save().unwrap();
        c.rectangle(track_x, track_y, fill_end - track_x, track_h);
        c.clip();
        c.set_source_surface(bar, track_x, track_y).unwrap();
        c.scale(track_w / bar.width() as f64, track_h / bar.height() as f64);
        c.paint().unwrap();
        c.restore().unwrap();
    } else {
        c.set_source_rgb(colors.bar_fill.0, colors.bar_fill.1, colors.bar_fill.2);
        if fill_end > track_x + r * 2.0 {
            c.new_sub_path();
            c.arc(fill_end - r, track_y + r, r, (-90.0f64).to_radians(), (0.0f64).to_radians());
            c.arc(fill_end - r, track_y + r, r, (0.0f64).to_radians(), (90.0f64).to_radians());
            c.arc(track_x + r, track_y + r, r, (90.0f64).to_radians(), (180.0f64).to_radians());
            c.arc(track_x + r, track_y + r, r, (180.0f64).to_radians(), (270.0f64).to_radians());
            c.close_path();
        } else {
            c.rectangle(track_x, track_y, fill_end - track_x, track_h);
        }
        c.fill().unwrap();
    }

    // Track outline (drawn before handle so handle appears on top)
    if let Some(ref col) = state.track_outline {
        col.set_cairo_source(c);
        c.set_line_width(1.0);
        c.new_sub_path();
        c.arc(track_x + track_w - r, track_y + r, r, (-90.0f64).to_radians(), 0.0);
        c.arc(track_x + track_w - r, track_y + r, r, 0.0, (90.0f64).to_radians());
        c.arc(track_x + r, track_y + r, r, (90.0f64).to_radians(), (180.0f64).to_radians());
        c.arc(track_x + r, track_y + r, r, (180.0f64).to_radians(), (270.0f64).to_radians());
        c.close_path();
        c.stroke().unwrap();
    }

    // Handle
    let hx = fill_end;
    let hy = cy;
    let hs = state.handle_size;
    if let Some(ref handle) = state.handle_surface {
        let hw = handle.width() as f64;
        let hh = handle.height() as f64;
        c.set_source_surface(handle, hx - hw / 2.0, hy - hh / 2.0).unwrap();
        c.rectangle(hx - hw / 2.0, hy - hh / 2.0, hw, hh);
        c.fill().unwrap();
    } else {
        let radius = hs / 2.0;
        c.set_source_rgb(colors.handle.0, colors.handle.1, colors.handle.2);
        c.arc(hx, hy, radius, 0.0, 360.0f64.to_radians());
        c.fill().unwrap();
        c.set_source_rgb(colors.handle_outline.0, colors.handle_outline.1, colors.handle_outline.2);
        c.set_line_width(1.5);
        c.arc(hx, hy, radius, 0.0, 360.0f64.to_radians());
        c.stroke().unwrap();
    }
}

impl WidgetKind {
    pub fn render_at(&mut self, c: &Context, x: f64, width: f64, height: f64, icon_size: f64) {
        match self {
            WidgetKind::Content(wc) => wc.render_at(c, x, width, height, icon_size),
            WidgetKind::Slider(state) => {
                // Render label (icon or text) left of the slider bar
                let lw = if let Some(ref label_icon) = state.label_icon {
                    label_icon.render_at(c, x, icon_size, height, icon_size);
                    icon_size
                } else if let Some(ref label_text) = state.label_text {
                    let extents = c.text_extents(label_text).unwrap();
                    let lw = extents.width() + 2.0 * extents.x_bearing().abs();
                    let font_extents = c.font_extents().unwrap();
                    let baseline_y = height / 2.0 + (font_extents.ascent() - font_extents.descent()) / 2.0;
                    c.move_to((x + lw / 2.0 - extents.width() / 2.0).round(), baseline_y.round());
                    c.show_text(label_text).unwrap();
                    lw
                } else {
                    0.0
                };
                state.label_w = lw;
                let slider_width = width - lw;
                if slider_width > 0.0 {
                    render_slider(state, c, x + lw, slider_width, height);
                }
            }
            WidgetKind::Container(state) => {
                let children_total: f64 = state.children.iter()
                    .map(|c| {
                        let pl = c.padding_left.map(|p| p as f64).unwrap_or(0.0);
                        let pr = c.padding_right.map(|p| p as f64).unwrap_or(0.0);
                        c.min_width + pl + pr
                    })
                    .sum();
                let auto_count = state.children.iter().filter(|c| c.padding_left.is_none()).count();
                let extra_each = if width > children_total && auto_count > 0 {
                    (width - children_total) / auto_count as f64
                } else {
                    0.0
                };
                let mut child_x = x;
                for child in &mut state.children {
                    let pl = child.padding_left.map(|p| p as f64).unwrap_or(0.0);
                    let pr = child.padding_right.map(|p| p as f64).unwrap_or(0.0);
                    let extra = if child.padding_left.is_none() { extra_each } else { 0.0 };
                    let child_w = child.min_width + pl + pr + extra;
                    child.kind.render_at(c, child_x + pl, child.min_width + extra, height, icon_size);
                    child_x += child_w;
                }
            }
        }
    }

    pub fn measure_min_width(&mut self, c: &Context, icon_size: f64) -> f64 {
        match self {
            WidgetKind::Content(wc) => wc.measure_min_width(c, icon_size),
            WidgetKind::Slider(state) => {
                let mut w = state.handle_size * 2.0;
                if state.label_icon.is_some() {
                    w += icon_size;
                } else if let Some(ref label_text) = state.label_text {
                    let ext = c.text_extents(label_text).unwrap();
                    w += ext.width() + 2.0 * ext.x_bearing().abs();
                }
                w
            }
            WidgetKind::Container(state) => {
                let mut total = 0.0;
                for child in &mut state.children {
                    child.measure_min_width(c, icon_size);
                    let pl = child.padding_left.map(|p| p as f64).unwrap_or(0.0);
                    let pr = child.padding_right.map(|p| p as f64).unwrap_or(0.0);
                    total += child.min_width + pl + pr;
                }
                total
            }
        }
    }
}

fn widget_min_width_impl(content: &WidgetContent, c: &Context, icon_size: f64) -> f64 {
    match content {
        WidgetContent::Text { text } => {
            let extents = c.text_extents(text).unwrap();
            extents.width() + 2.0 * extents.x_bearing().abs()
        }
        WidgetContent::Svg { .. } | WidgetContent::Bitmap { .. } => icon_size,
        WidgetContent::Time { format, locale } => {
            let formatted = Local::now()
                .format_localized_with_items(format.iter(), *locale)
                .to_string();
            let extents = c.text_extents(&formatted).unwrap();
            extents.width() + 2.0 * extents.x_bearing().abs()
        }
        WidgetContent::Battery { battery, mode, .. } => {
            let (capacity, state) = get_battery_state(battery);
            let percent_str = format!("{:.0}%", capacity);
            let extents = c.text_extents(&percent_str).unwrap();
            let mut w = extents.width() + 2.0 * extents.x_bearing().abs();
            if mode.should_draw_icon() || state == BatteryState::Charging {
                w += icon_size;
            }
            w
        }
    }
}

impl WidgetContent {
    fn measure_min_width(&self, c: &Context, icon_size: f64) -> f64 {
        widget_min_width_impl(self, c, icon_size)
    }
}

#[derive(Default, Clone)]
pub struct FunctionLayer {
    pub displays_time: bool,
    pub displays_battery: bool,
    pub widgets: Vec<(usize, Widget)>,
}

struct WidgetLayout {
    drawn_widths: Vec<f64>,
    margin_lefts: Vec<f64>,
    margin_rights: Vec<f64>,
    slot_widths: Vec<f64>,
    extra_each: f64,
}

impl FunctionLayer {
    pub fn get_widget_mut(&mut self, idx: usize, child_idx: Option<usize>) -> &mut Widget {
        let w = &mut self.widgets[idx].1;
        if let Some(c_idx) = child_idx {
            if let WidgetKind::Container(ref mut state) = w.kind {
                return &mut state.children[c_idx];
            }
        }
        w
    }

    pub fn get_widget(&self, idx: usize, child_idx: Option<usize>) -> &Widget {
        let w = &self.widgets[idx].1;
        if let Some(c_idx) = child_idx {
            if let WidgetKind::Container(ref state) = w.kind {
                return &state.children[c_idx];
            }
        }
        w
    }

    fn compute_layout(&self, width: f64) -> WidgetLayout {
        let drawn_widths: Vec<f64> = self.widgets.iter()
            .map(|(_, w)| {
                let pl = w.padding_left.map(|p| p as f64).unwrap_or(0.0);
                let pr = w.padding_right.map(|p| p as f64).unwrap_or(0.0);
                w.min_width + pl + pr
            })
            .collect();
        let margin_lefts: Vec<f64> = self.widgets.iter()
            .map(|(_, w)| w.margin_left as f64)
            .collect();
        let margin_rights: Vec<f64> = self.widgets.iter()
            .map(|(_, w)| w.margin_right as f64)
            .collect();
        let slot_widths: Vec<f64> = (0..self.widgets.len())
            .map(|i| drawn_widths[i] + margin_lefts[i] + margin_rights[i])
            .collect();
        let total_slots: f64 = slot_widths.iter().sum();
        let remaining = width - total_slots;
        let auto_count = self.widgets.iter().filter(|(_, w)| w.padding_left.is_none()).count();
        let extra_each = if remaining > 0.0 && auto_count > 0 { remaining / auto_count as f64 } else { 0.0 };
        WidgetLayout { drawn_widths, margin_lefts, margin_rights, slot_widths, extra_each }
    }

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
    pub fn draw(
        &mut self,
        config: &Config,
        width: i32,
        height: i32,
        surface: &Surface,
        pixel_shift: (f64, f64),
        complete_redraw: bool,
    ) -> Vec<ClipRect> {
        let c = Context::new(surface).unwrap();
        c.translate(height as f64, 0.0);
        c.rotate((90.0f64).to_radians());

        let radius = 8.0f64;
        let bot = (height as f64) * 0.15;
        let top = (height as f64) * 0.85;
        let (pixel_shift_x, _) = pixel_shift;

        if complete_redraw {
            c.set_source_rgb(0.0, 0.0, 0.0);
            c.paint().unwrap();
        }

        c.set_font_face(&config.font_face);
        c.set_font_size(config.font_size);

        // Measure min_width for all widgets
        for (_, w) in &mut self.widgets {
            if let Some(fs) = w.font_size {
                c.set_font_size(fs);
            }
            w.measure_min_width(&c, w.icon_size.unwrap_or(config.icon_size));
        }
        c.set_font_size(config.font_size);

        let n = self.widgets.len();
        let available = width as f64;

        let layout = self.compute_layout(available);

        // Cascade changed flag: if widget i's size changed, all widgets from i onward shift
        if !complete_redraw {
            let first_changed = self.widgets.iter().position(|(_, w)| {
                if w.changed {
                    return true;
                }
                if let WidgetKind::Container(ref state) = w.kind {
                    return state.children.iter().any(|c| c.changed);
                }
                false
            });
            if let Some(first) = first_changed {
                for (_, w) in self.widgets.iter_mut().skip(first) {
                    w.changed = true;
                }
            }
        }

        let mut modified = if complete_redraw {
            vec![ClipRect::new(0, 0, height as u16, width as u16)]
        } else {
            Vec::with_capacity(n)
        };

        let mut x_offset = pixel_shift_x;

        for i in 0..n {
            let ml = layout.margin_lefts[i];
            let mr = layout.margin_rights[i];
            let extra = if self.widgets[i].1.padding_left.is_none() { layout.extra_each } else { 0.0 };
            let dw = layout.drawn_widths[i] + extra;
            let sw = dw + ml + mr;
            let le = x_offset + ml;

            let container_changed = match &self.widgets[i].1.kind {
                WidgetKind::Container(state) => {
                    complete_redraw || self.widgets[i].1.changed
                        || state.children.iter().any(|c| c.changed)
                }
                _ => self.widgets[i].1.changed,
            };
            let redraw = complete_redraw || container_changed;

            if redraw {
                // Erase old content behind this widget
                if !complete_redraw {
                    c.set_source_rgb(0.0, 0.0, 0.0);
                    c.rectangle(le, bot - radius, dw, top - bot + radius * 2.0);
                    c.fill().unwrap();
                }

                let show_outline = self.widgets[i].1.show_outline.unwrap_or(config.show_outline);

                if matches!(self.widgets[i].1.kind, WidgetKind::Container(_)) {
                    // Container: draw outline around the group, then render children
                    if show_outline {
                        if self.widgets[i].1.active {
                            if let Some(ref col) = self.widgets[i].1.active_color {
                                col.set_cairo_source(&c);
                            } else {
                                c.set_source_rgb(BUTTON_COLOR_ACTIVE, BUTTON_COLOR_ACTIVE, BUTTON_COLOR_ACTIVE);
                            }
                        } else {
                            if let Some(ref col) = self.widgets[i].1.outline_color {
                                col.set_cairo_source(&c);
                            } else {
                                c.set_source_rgb(BUTTON_COLOR_INACTIVE, BUTTON_COLOR_INACTIVE, BUTTON_COLOR_INACTIVE);
                            }
                        }
                        c.new_sub_path();
                        let lx = le + radius;
                        let rx = (le + dw.ceil()) - radius;
                        c.arc(rx, bot, radius, (-90.0f64).to_radians(), (0.0f64).to_radians());
                        c.arc(rx, top, radius, (0.0f64).to_radians(), (90.0f64).to_radians());
                        c.arc(lx, top, radius, (90.0f64).to_radians(), (180.0f64).to_radians());
                        c.arc(lx, bot, radius, (180.0f64).to_radians(), (270.0f64).to_radians());
                        c.close_path();
                        c.fill().unwrap();
                    }

                    // Render children
                    c.set_source_rgb(1.0, 1.0, 1.0);
                    c.set_font_size(config.font_size);
                    if let WidgetKind::Container(ref mut state) = &mut self.widgets[i].1.kind {
                        let children_total: f64 = state.children.iter()
                            .map(|c| {
                                let pl = c.padding_left.map(|p| p as f64).unwrap_or(0.0);
                                let pr = c.padding_right.map(|p| p as f64).unwrap_or(0.0);
                                c.min_width + pl + pr
                            })
                            .sum();
                        let auto_count = state.children.iter().filter(|c| c.padding_left.is_none()).count();
                        let extra_each = if dw > children_total && auto_count > 0 {
                            (dw - children_total) / auto_count as f64
                        } else {
                            0.0
                        };
                        let mut child_x = le;
                        for child in &mut state.children {
                            if let Some(fs) = child.font_size {
                                c.set_font_size(fs);
                            }
                            let pl = child.padding_left.map(|p| p as f64).unwrap_or(0.0);
                            let pr = child.padding_right.map(|p| p as f64).unwrap_or(0.0);
                            let extra = if child.padding_left.is_none() { extra_each } else { 0.0 };
                            let child_w = child.min_width + pl + pr + extra;
                            let is = child.icon_size.unwrap_or(config.icon_size);
                            child.kind.render_at(&c, child_x + pl, child.min_width + extra, height as f64, is);
                            child_x += child_w;
                            child.changed = false;
                        }
                        c.set_font_size(config.font_size);
                    }
                } else {
                    // Normal widget: background + outline + content
                    if self.widgets[i].1.active {
                        if let Some(ref col) = self.widgets[i].1.active_color {
                            col.set_cairo_source(&c);
                        } else {
                            self.widgets[i].1.set_background_color(&c, BUTTON_COLOR_ACTIVE);
                        }
                    } else if show_outline {
                        if let Some(ref col) = self.widgets[i].1.outline_color {
                            col.set_cairo_source(&c);
                        } else {
                            self.widgets[i].1.set_background_color(&c, BUTTON_COLOR_INACTIVE);
                        }
                    } else {
                        self.widgets[i].1.set_background_color(&c, 0.0);
                    }

                    // Rounded rect
                    c.new_sub_path();
                    let lx = le + radius;
                    let rx = (le + dw.ceil()) - radius;
                    c.arc(rx, bot, radius, (-90.0f64).to_radians(), (0.0f64).to_radians());
                    c.arc(rx, top, radius, (0.0f64).to_radians(), (90.0f64).to_radians());
                    c.arc(lx, top, radius, (90.0f64).to_radians(), (180.0f64).to_radians());
                    c.arc(lx, bot, radius, (180.0f64).to_radians(), (270.0f64).to_radians());
                    c.close_path();
                    c.fill().unwrap();

                    // Content (white text/icons centered within drawn_width)
                    c.set_source_rgb(1.0, 1.0, 1.0);
                    if let Some(fs) = self.widgets[i].1.font_size {
                        c.set_font_size(fs);
                    } else {
                        c.set_font_size(config.font_size);
                    }
                    let is = self.widgets[i].1.icon_size.unwrap_or(config.icon_size);
                    self.widgets[i].1.kind.render_at(&c, le, dw, height as f64, is);
                }

                // Track clip region for DRM
                if !complete_redraw {
                    modified.push(ClipRect::new(
                        height as u16 - top as u16 - radius as u16,
                        le as u16,
                        height as u16 - bot as u16 + radius as u16,
                        le as u16 + dw as u16,
                    ));
                }

                self.widgets[i].1.changed = false;
            }

            x_offset += sw;
        }

        modified
    }

    pub fn child_position(&self, width: u16, index: usize, child_index: Option<usize>) -> Option<(f64, f64)> {
        let (le, dw) = self.widget_position(width, index)?;
        if let Some(c_idx) = child_index {
            if let WidgetKind::Container(ref state) = self.widgets[index].1.kind {
                let child = state.children.get(c_idx)?;
                let children_total: f64 = state.children.iter()
                    .map(|c| {
                        let pl = c.padding_left.map(|p| p as f64).unwrap_or(0.0);
                        let pr = c.padding_right.map(|p| p as f64).unwrap_or(0.0);
                        c.min_width + pl + pr
                    })
                    .sum();
                let auto_count = state.children.iter().filter(|c| c.padding_left.is_none()).count();
                let extra_each = if dw > children_total && auto_count > 0 {
                    (dw - children_total) / auto_count as f64
                } else {
                    0.0
                };
                let child_x = le + state.children[..c_idx].iter().map(|c| {
                    let pl = c.padding_left.map(|p| p as f64).unwrap_or(0.0);
                    let pr = c.padding_right.map(|p| p as f64).unwrap_or(0.0);
                    let extra = if c.padding_left.is_none() { extra_each } else { 0.0 };
                    c.min_width + pl + pr + extra
                }).sum::<f64>();
                let pl = child.padding_left.map(|p| p as f64).unwrap_or(0.0);
                let extra = if child.padding_left.is_none() { extra_each } else { 0.0 };
                return Some((child_x + pl, child.min_width + extra));
            }
        }
        Some((le, dw))
    }

    pub fn widget_position(&self, width: u16, index: usize) -> Option<(f64, f64)> {
        let n = self.widgets.len();
        if index >= n {
            return None;
        }

        let layout = self.compute_layout(width as f64);

        let mut x_offset = 0.0;
        for i in 0..n {
            let extra = if self.widgets[i].1.padding_left.is_none() { layout.extra_each } else { 0.0 };
            let drawn_width = layout.drawn_widths[i] + extra;
            let slot_width = layout.slot_widths[i] + extra;
            let left_edge = x_offset + layout.margin_lefts[i];

            if i == index {
                return Some((left_edge, drawn_width));
            }

            if i < n - 1 {
                x_offset += slot_width;
            }
        }
        None
    }

    pub fn hit(&self, width: u16, height: u16, x: f64, y: f64, i: Option<usize>) -> Option<(usize, Option<usize>)> {
        if let Some(idx) = i {
            return self.widgets.get(idx).and_then(|(_, w)| {
                if y >= 0.1 * height as f64 && y <= 0.9 * height as f64 {
                    // If this is a container, check which child was hit
                    if let WidgetKind::Container(ref state) = w.kind {
                        let children_total: f64 = state.children.iter()
                            .map(|c| {
                                let pl = c.padding_left.map(|p| p as f64).unwrap_or(0.0);
                                let pr = c.padding_right.map(|p| p as f64).unwrap_or(0.0);
                                c.min_width + pl + pr
                            })
                            .sum();
                        let auto_count = state.children.iter().filter(|c| c.padding_left.is_none()).count();
                        let available = self.widget_position(width, idx).map(|(_, dw)| dw).unwrap_or(children_total);
                        let extra_each = if available > children_total && auto_count > 0 {
                            (available - children_total) / auto_count as f64
                        } else {
                            0.0
                        };
                        let mut child_x = 0.0;
                        for (ci, child) in state.children.iter().enumerate() {
                            let pl = child.padding_left.map(|p| p as f64).unwrap_or(0.0);
                            let pr = child.padding_right.map(|p| p as f64).unwrap_or(0.0);
                            let extra = if child.padding_left.is_none() { extra_each } else { 0.0 };
                            let child_w = child.min_width + pl + pr + extra;
                            if x >= child_x && x <= child_x + child_w {
                                return Some((idx, Some(ci)));
                            }
                            child_x += child_w;
                        }
                        return Some((idx, None));
                    }
                    Some((idx, None))
                } else {
                    None
                }
            });
        }

        if y < 0.1 * height as f64 || y > 0.9 * height as f64 {
            return None;
        }

        let last = self.widgets.len().saturating_sub(1);
        let n = self.widgets.len();

        let layout = self.compute_layout(width as f64);

        let mut x_offset = 0.0;

        for i in 0..n {
            let extra = if self.widgets[i].1.padding_left.is_none() { layout.extra_each } else { 0.0 };
            let drawn_width = layout.drawn_widths[i] + extra;
            let slot_width = layout.slot_widths[i] + extra;
            let left_edge = x_offset + layout.margin_lefts[i];

            if x >= left_edge && x <= left_edge + drawn_width {
                // Check container children
                if let WidgetKind::Container(ref state) = self.widgets[i].1.kind {
                    let children_total: f64 = state.children.iter()
                        .map(|c| {
                            let pl = c.padding_left.map(|p| p as f64).unwrap_or(0.0);
                            let pr = c.padding_right.map(|p| p as f64).unwrap_or(0.0);
                            c.min_width + pl + pr
                        })
                        .sum();
                    let auto_count = state.children.iter().filter(|c| c.padding_left.is_none()).count();
                    let extra_each = if drawn_width > children_total && auto_count > 0 {
                        (drawn_width - children_total) / auto_count as f64
                    } else {
                        0.0
                    };
                    let rel_x = x - left_edge;
                    let mut child_x = 0.0;
                    for (ci, child) in state.children.iter().enumerate() {
                        let pl = child.padding_left.map(|p| p as f64).unwrap_or(0.0);
                        let pr = child.padding_right.map(|p| p as f64).unwrap_or(0.0);
                        let extra = if child.padding_left.is_none() { extra_each } else { 0.0 };
                        let child_w = child.min_width + pl + pr + extra;
                        if rel_x >= child_x && rel_x <= child_x + child_w {
                            return Some((i, Some(ci)));
                        }
                        child_x += child_w;
                    }
                }
                return Some((i, None));
            }

            if i < last {
                x_offset += slot_width;
            }
        }

        None
    }
}
