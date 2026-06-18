// SPDX-License-Identifier: GPL-3.0-only
use cairo::{Context, ImageSurface};
use chrono::{
    format::{Item as ChronoItem, Locale, StrftimeItems},
    Local,
};
use librsvg_rebind::{prelude::HandleExt, Handle, Rectangle};
use super::battery::{get_battery_state, BatteryState};
use super::icon::{try_load_image, load_battery_image, make_placeholder_icon};

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

impl WidgetContent {
    pub fn new_icon(name: impl AsRef<str>, theme: Option<impl AsRef<str>>) -> WidgetContent {
        match try_load_image(name.as_ref(), theme) {
            Ok((handle, None)) => WidgetContent::Svg { handle },
            Ok((_, Some(surface))) => WidgetContent::Bitmap { surface },
            Err(_) => WidgetContent::Bitmap { surface: make_placeholder_icon() },
        }
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

    pub(crate) fn measure_min_width(&self, c: &Context, icon_size: f64) -> f64 {
        match self {
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


