// SPDX-License-Identifier: GPL-3.0-only
use std::collections::HashMap;
use std::ops::Deref;
use std::path::PathBuf;
use std::process::Command;
use std::fs;
use std::sync::OnceLock;

static BRIGHTNESS_MAX: OnceLock<i32> = OnceLock::new();
static KBD_BRIGHTNESS_MAX: OnceLock<i32> = OnceLock::new();

use cairo::{Context, ImageSurface};

use crate::config::{OutlineColor, SliderType};
use crate::paths::slider_icons_dir;

// ── Colors ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SliderColors {
    pub handle: (f64, f64, f64),
    pub handle_outline: (f64, f64, f64),
    pub bar_fill: (f64, f64, f64),
    pub bar_empty: (f64, f64, f64),
}

impl Default for SliderColors {
    fn default() -> Self {
        SliderColors {
            handle: (1.0, 1.0, 1.0),
            handle_outline: (0.5, 0.5, 0.5),
            bar_fill: (0.3, 0.6, 1.0),
            bar_empty: (0.2, 0.2, 0.2),
        }
    }
}

// ── Slider state ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SliderState {
    pub slider_type: SliderType,
    pub value: f64,
    pub handle_surface: Option<ImageSurface>,
    pub bar_surface: Option<ImageSurface>,
    pub handle_size: f64,
    pub colors: SliderColors,
    pub label_icon: Option<super::content::WidgetContent>,
    pub label_text: Option<String>,
    pub dragging: bool,
    pub label_w: f64,
    pub live_update: bool,
    pub track_outline: Option<OutlineColor>,
}

// ── Image loading ───────────────────────────────────────────────

fn load_slider_icon(path: &str, size: f64) -> Option<ImageSurface> {
    let p = if path.starts_with('/') {
        PathBuf::from(path)
    } else {
        slider_icons_dir().join(path)
    };
    if !p.exists() {
        return None;
    }
    crate::widget::icon::load_image_surface(&p, size).ok()
}

// ── Backend: Volume (PulseAudio D-Bus) ──────────────────────────

const PA_VOLUME_NORM: f64 = 65536.0;

fn session_conn() -> Result<zbus::blocking::Connection, anyhow::Error> {
    Ok(zbus::blocking::Connection::session()?)
}

fn read_volume() -> (f64, String) {
    let conn = match session_conn() {
        Ok(c) => c,
        Err(_) => return (0.0, "N/A".into()),
    };
    let sinks: Vec<String> = match conn.call_method(
        Some("org.PulseAudio.Core1"),
        "/org/pulseaudio/core1",
        Some("org.PulseAudio.Core1"),
        "GetSinks",
        &(),
    ).and_then(|r| r.body()) {
        Ok(s) => s,
        Err(_) => return (0.0, "N/A".into()),
    };
    let sink_path = match sinks.first() {
        Some(p) => p,
        None => return (0.0, "N/A".into()),
    };
    let vol_value: zbus::zvariant::OwnedValue = match conn.call_method(
        Some("org.PulseAudio.Core1"),
        sink_path.as_str(),
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.PulseAudio.Sink1", "Volume"),
    ).and_then(|r| r.body()) {
        Ok(v) => v,
        Err(_) => return (0.0, "N/A".into()),
    };
    let volumes: Vec<u32> = match vol_value.deref() {
        zbus::zvariant::Value::Array(arr) => {
            arr.iter().filter_map(|v| {
                if let zbus::zvariant::Value::U32(n) = v { Some(*n) } else { None }
            }).collect()
        }
        _ => return (0.0, "N/A".into()),
    };
    if volumes.is_empty() {
        return (0.0, "N/A".into());
    }
    let avg = volumes.iter().copied().sum::<u32>() as f64 / volumes.len() as f64;
    let fraction = (avg / PA_VOLUME_NORM).clamp(0.0, 1.0);
    let pct = (fraction * 100.0).round();
    (fraction, format!("{:.0}%", pct))
}

fn write_volume(value: f64) {
    let conn = match session_conn() {
        Ok(c) => c,
        Err(_) => return,
    };
    let sinks: Vec<String> = match conn.call_method(
        Some("org.PulseAudio.Core1"),
        "/org/pulseaudio/core1",
        Some("org.PulseAudio.Core1"),
        "GetSinks",
        &(),
    ).and_then(|r| r.body()) {
        Ok(s) => s,
        Err(_) => return,
    };
    let sink_path = match sinks.first() {
        Some(p) => p,
        None => return,
    };
    let vol_value: zbus::zvariant::OwnedValue = match conn.call_method(
        Some("org.PulseAudio.Core1"),
        sink_path.as_str(),
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.PulseAudio.Sink1", "Volume"),
    ).and_then(|r| r.body()) {
        Ok(v) => v,
        Err(_) => return,
    };
    let n_channels = match vol_value.deref() {
        zbus::zvariant::Value::Array(arr) => arr.len().max(1),
        _ => 2,
    };
    let vol = (value * PA_VOLUME_NORM).round().clamp(0.0, PA_VOLUME_NORM) as u32;
    let volumes = vec![vol; n_channels];
    let _ = conn.call_method(
        Some("org.PulseAudio.Core1"),
        sink_path.as_str(),
        Some("org.PulseAudio.Sink1"),
        "SetVolume",
        &volumes,
    );
}

// ── Backend: Brightness (D-Bus) ─────────────────────────────────

const BRIGHTNESS_SERVICE: &str = "org.kde.Solid.PowerManagement";
const BRIGHTNESS_PATH: &str = "/org/kde/Solid/PowerManagement/Actions/BrightnessControl";
const BRIGHTNESS_IFACE: &str = "org.kde.Solid.PowerManagement.Actions.BrightnessControl";

const KBD_BRIGHTNESS_PATH: &str = "/org/kde/Solid/PowerManagement/Actions/KeyboardBrightnessControl";
const KBD_BRIGHTNESS_IFACE: &str = "org.kde.Solid.PowerManagement.Actions.KeyboardBrightnessControl";

fn read_brightness_max(conn: &zbus::blocking::Connection) -> i32 {
    *BRIGHTNESS_MAX.get_or_init(|| {
        let max: Result<i32, _> = conn.call_method(
            Some(BRIGHTNESS_SERVICE),
            BRIGHTNESS_PATH,
            Some(BRIGHTNESS_IFACE),
            "brightnessMax",
            &(),
        ).and_then(|r| r.body());
        max.unwrap_or(100)
    })
}

fn read_dbus_brightness() -> (f64, String) {
    let conn = match session_conn() {
        Ok(c) => c,
        Err(_) => return (0.0, "N/A".into()),
    };
    let cur: Result<i32, _> = conn.call_method(
        Some(BRIGHTNESS_SERVICE),
        BRIGHTNESS_PATH,
        Some(BRIGHTNESS_IFACE),
        "brightness",
        &(),
    ).and_then(|r| r.body());
    let max = read_brightness_max(&conn);
    match cur {
        Ok(c) if max > 0 => {
            let val = c as f64 / max as f64;
            (val, format!("{:.0}%", val * 100.0))
        }
        _ => (0.0, "N/A".into()),
    }
}

fn write_dbus_brightness(value: f64) {
    let conn = match session_conn() {
        Ok(c) => c,
        Err(_) => return,
    };
    let max = read_brightness_max(&conn);
    let abs = (value * max as f64).round().clamp(0.0, max as f64) as i32;
    let _ = conn.call_method(
        Some(BRIGHTNESS_SERVICE),
        BRIGHTNESS_PATH,
        Some(BRIGHTNESS_IFACE),
        "setBrightness",
        &(abs,),
    );
}

fn read_kbd_brightness_max(conn: &zbus::blocking::Connection) -> i32 {
    *KBD_BRIGHTNESS_MAX.get_or_init(|| {
        let max: Result<i32, _> = conn.call_method(
            Some(BRIGHTNESS_SERVICE),
            KBD_BRIGHTNESS_PATH,
            Some(KBD_BRIGHTNESS_IFACE),
            "keyboardBrightnessMax",
            &(),
        ).and_then(|r| r.body());
        max.unwrap_or(100)
    })
}

fn read_dbus_keyboard_brightness() -> (f64, String) {
    let conn = match session_conn() {
        Ok(c) => c,
        Err(_) => return (0.0, "N/A".into()),
    };
    let cur: Result<i32, _> = conn.call_method(
        Some(BRIGHTNESS_SERVICE),
        KBD_BRIGHTNESS_PATH,
        Some(KBD_BRIGHTNESS_IFACE),
        "keyboardBrightness",
        &(),
    ).and_then(|r| r.body());
    let max = read_kbd_brightness_max(&conn);
    match cur {
        Ok(c) if max > 0 => {
            let val = c as f64 / max as f64;
            (val, format!("{:.0}%", val * 100.0))
        }
        _ => (0.0, "N/A".into()),
    }
}

fn write_dbus_keyboard_brightness(value: f64) {
    let conn = match session_conn() {
        Ok(c) => c,
        Err(_) => return,
    };
    let max = read_kbd_brightness_max(&conn);
    let abs = (value * max as f64).round().clamp(0.0, max as f64) as i32;
    let _ = conn.call_method(
        Some(BRIGHTNESS_SERVICE),
        KBD_BRIGHTNESS_PATH,
        Some(KBD_BRIGHTNESS_IFACE),
        "setKeyboardBrightness",
        &(abs,),
    );
}

// ── Public API ──────────────────────────────────────────────────

pub fn read_slider_value(stype: SliderType) -> (f64, String) {
    match stype {
        SliderType::Volume => read_volume(),
        SliderType::Brightness => read_dbus_brightness(),
        SliderType::KeyboardBrightness => read_dbus_keyboard_brightness(),
    }
}

pub fn write_slider_value(stype: SliderType, value: f64) {
    match stype {
        SliderType::Volume => write_volume(value),
        SliderType::Brightness => write_dbus_brightness(value),
        SliderType::KeyboardBrightness => write_dbus_keyboard_brightness(value),
    }
}

// ── Slider factory ──────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn new_slider_state(
    slider_type: SliderType,
    handle_icon: Option<&str>,
    bar_icon: Option<&str>,
    handle_size: f64,
    label_icon: Option<super::content::WidgetContent>,
    label_text: Option<String>,
    colors: SliderColors,
    live_update: bool,
    track_outline: Option<OutlineColor>,
) -> SliderState {
    let (value, _display_text) = read_slider_value(slider_type);
    let handle_surface = handle_icon.and_then(|s| load_slider_icon(s, handle_size));
    let bar_surface = bar_icon.and_then(|s| load_slider_icon(s, 48.0));
    SliderState {
        slider_type, value, handle_surface, bar_surface, handle_size,
        colors, label_icon, label_text, dragging: false, label_w: 0.0, live_update,
        track_outline,
    }
}

// ── Slider rendering ────────────────────────────────────────────

pub fn render_slider(state: &SliderState, c: &Context, x: f64, width: f64, height: f64) {
    let cy = height / 2.0;
    let track_w = width * 0.85;
    let track_x = x + (width - track_w) / 2.0;
    let track_h = 6.0f64.max(height * 0.06);
    let track_y = cy - track_h / 2.0;
    let value = state.value;
    let fill_end = track_x + track_w * value;
    let colors = &state.colors;

    c.set_source_rgb(colors.bar_empty.0, colors.bar_empty.1, colors.bar_empty.2);
    c.new_sub_path();
    let r = track_h / 2.0;
    c.arc(track_x + track_w - r, track_y + r, r, (-90.0f64).to_radians(), (0.0f64).to_radians());
    c.arc(track_x + track_w - r, track_y + r, r, (0.0f64).to_radians(), (90.0f64).to_radians());
    c.arc(track_x + r, track_y + r, r, (90.0f64).to_radians(), (180.0f64).to_radians());
    c.arc(track_x + r, track_y + r, r, (180.0f64).to_radians(), (270.0f64).to_radians());
    c.close_path();
    c.fill().unwrap();

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

// ── Color theme loading ─────────────────────────────────────────

fn parse_kdeglobals() -> Option<HashMap<String, HashMap<String, String>>> {
    let path = std::env::var("HOME").ok().map(|h| {
        PathBuf::from(h).join(".config/kdeglobals")
    })?;
    let content = fs::read_to_string(&path).ok()?;
    let mut result: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current_section = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current_section = line[1..line.len()-1].to_string();
            result.entry(current_section.clone()).or_default();
        } else if let Some((key, val)) = line.split_once('=') {
            if !current_section.is_empty() {
                if let Some(section) = result.get_mut(&current_section) {
                    section.insert(key.trim().to_string(), val.trim().to_string());
                }
            }
        }
    }
    Some(result)
}

fn parse_color_hex(s: &str) -> Option<(f64, f64, f64)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() == 6 {
        let r = u8::from_str_radix(&s[0..2], 16).ok()?;
        let g = u8::from_str_radix(&s[2..4], 16).ok()?;
        let b = u8::from_str_radix(&s[4..6], 16).ok()?;
        Some((r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0))
    } else {
        None
    }
}

fn parse_color_rgb(s: &str) -> Option<(f64, f64, f64)> {
    let mut parts = s.trim().split(',');
    let r: f64 = parts.next()?.trim().parse().ok()?;
    let g: f64 = parts.next()?.trim().parse().ok()?;
    let b: f64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((r / 255.0, g / 255.0, b / 255.0))
}

fn parse_color(s: &str) -> Option<(f64, f64, f64)> {
    parse_color_hex(s).or_else(|| parse_color_rgb(s))
}

pub fn load_slider_colors(color_source: &str) -> SliderColors {
    match color_source {
        "gtk" => load_gtk_colors().unwrap_or_default(),
        _ => load_kde_colors().unwrap_or_default(),
    }
}

fn load_kde_colors() -> Option<SliderColors> {
    let globals = parse_kdeglobals()?;
    let view = globals.get("Colors:View")?;
    let handle = view.get("ForegroundNormal")
        .and_then(|s| parse_color(s))
        .unwrap_or((1.0, 1.0, 1.0));
    let handle_outline = view.get("DecorationHover")
        .and_then(|s| parse_color(s))
        .unwrap_or((0.5, 0.5, 0.5));
    let bar_fill = view.get("BackgroundSelected")
        .and_then(|s| parse_color(s))
        .or_else(|| {
            globals.get("Colors:Selection")
                .and_then(|s| s.get("BackgroundNormal"))
                .and_then(|s| parse_color(s))
        })
        .unwrap_or((0.3, 0.6, 1.0));
    let bar_empty = view.get("BackgroundNormal")
        .and_then(|s| parse_color(s))
        .map(|(r, g, b)| (r * 0.4, g * 0.4, b * 0.4))
        .unwrap_or((0.2, 0.2, 0.2));
    Some(SliderColors { handle, handle_outline, bar_fill, bar_empty })
}

fn load_gtk_colors() -> Option<SliderColors> {
    let theme = get_current_gtk_theme()?;
    let theme_dir = PathBuf::from("/usr/share/themes").join(&theme);
    let mut css_file = theme_dir.join("gtk-3.0/gtk.css");
    if !css_file.exists() {
        css_file = theme_dir.join("gtk-4.0/gtk.css");
    }
    if !css_file.exists() {
        return None;
    }
    let css = fs::read_to_string(&css_file).ok()?;
    let mut colors: HashMap<String, (f64, f64, f64)> = HashMap::new();
    for line in css.lines() {
        let line = line.trim();
        if let Some(def) = line.strip_prefix("@define-color ") {
            if let Some((name, hex)) = def.split_once(char::is_whitespace) {
                if let Some(color) = parse_color_hex(hex.trim().trim_end_matches(';')) {
                    colors.insert(name.to_string(), color);
                }
            }
        }
    }
    let bg = colors.get("theme_bg_color").copied();
    let fg = colors.get("theme_fg_color").copied();
    let selected_bg = colors.get("theme_selected_bg_color").copied();
    let hover = colors.get("theme_hover_bg_color").copied().or(colors.get("theme_unfocused_bg_color").copied());
    Some(SliderColors {
        handle: fg.unwrap_or((1.0, 1.0, 1.0)),
        handle_outline: hover.unwrap_or((0.5, 0.5, 0.5)),
        bar_fill: selected_bg.unwrap_or((0.3, 0.6, 1.0)),
        bar_empty: bg.map(|(r, g, b)| (r * 0.4, g * 0.4, b * 0.4)).unwrap_or((0.2, 0.2, 0.2)),
    })
}

fn get_current_gtk_theme() -> Option<String> {
    if let Ok(out) = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "gtk-theme"])
        .output()
    {
        let s = String::from_utf8(out.stdout).ok()?;
        let theme = s.trim().trim_matches('\'');
        if !theme.is_empty() && theme != "default" {
            return Some(theme.to_string());
        }
    }
    let globals = parse_kdeglobals()?;
    let general = globals.get("General")?;
    let theme = general.get("gtkTheme")?.to_string();
    if !theme.is_empty() {
        return Some(theme);
    }
    None
}
