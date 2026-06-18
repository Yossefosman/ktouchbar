// SPDX-License-Identifier: GPL-3.0-only
use cairo::{Context, Surface};
use drm::control::ClipRect;

use super::slider;
use super::{FunctionLayer, Widget, WidgetKind};
use crate::config::Config;

const BUTTON_COLOR_INACTIVE: f64 = 0.200;
const BUTTON_COLOR_ACTIVE: f64 = 0.400;

struct WidgetLayout {
    drawn_widths: Vec<f64>,
    margin_lefts: Vec<f64>,
    margin_rights: Vec<f64>,
    slot_widths: Vec<f64>,
    extra_each: f64,
}

impl WidgetKind {
    pub fn render_at(&mut self, c: &Context, x: f64, width: f64, height: f64, icon_size: f64) {
        match self {
            WidgetKind::Content(wc) => wc.render_at(c, x, width, height, icon_size),
            WidgetKind::Slider(state) => {
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
                    slider::render_slider(state, c, x + lw, slider_width, height);
                }
            }
            WidgetKind::Container(state) => {
                let extra_each = container_extra_each(&state.children, width);
                let mut child_x = x;
                for child in &mut state.children {
                    let (pl, _pr, extra) = container_child_slot(child, extra_each);
                    let child_w = child.min_width + pl + _pr + extra;
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
                if !complete_redraw {
                    c.set_source_rgb(0.0, 0.0, 0.0);
                    c.rectangle(le, bot - radius, dw, top - bot + radius * 2.0);
                    c.fill().unwrap();
                }

                let show_outline = self.widgets[i].1.show_outline.unwrap_or(config.show_outline);

                if matches!(self.widgets[i].1.kind, WidgetKind::Container(_)) {
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

                    c.set_source_rgb(1.0, 1.0, 1.0);
                    c.set_font_size(config.font_size);
                    if let WidgetKind::Container(ref mut state) = &mut self.widgets[i].1.kind {
                        let extra_each = container_extra_each(&state.children, dw);
                        let mut child_x = le;
                        for child in &mut state.children {
                            if let Some(fs) = child.font_size {
                                c.set_font_size(fs);
                            }
                            let (pl, _pr, extra) = container_child_slot(child, extra_each);
                            let child_w = child.min_width + pl + _pr + extra;
                            let is = child.icon_size.unwrap_or(config.icon_size);
                            child.kind.render_at(&c, child_x + pl, child.min_width + extra, height as f64, is);
                            child_x += child_w;
                            child.changed = false;
                        }
                        c.set_font_size(config.font_size);
                    }
                } else {
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

                    c.new_sub_path();
                    let lx = le + radius;
                    let rx = (le + dw.ceil()) - radius;
                    c.arc(rx, bot, radius, (-90.0f64).to_radians(), (0.0f64).to_radians());
                    c.arc(rx, top, radius, (0.0f64).to_radians(), (90.0f64).to_radians());
                    c.arc(lx, top, radius, (90.0f64).to_radians(), (180.0f64).to_radians());
                    c.arc(lx, bot, radius, (180.0f64).to_radians(), (270.0f64).to_radians());
                    c.close_path();
                    c.fill().unwrap();

                    c.set_source_rgb(1.0, 1.0, 1.0);
                    if let Some(fs) = self.widgets[i].1.font_size {
                        c.set_font_size(fs);
                    } else {
                        c.set_font_size(config.font_size);
                    }
                    let is = self.widgets[i].1.icon_size.unwrap_or(config.icon_size);
                    self.widgets[i].1.kind.render_at(&c, le, dw, height as f64, is);
                }

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
                let extra_each = container_extra_each(&state.children, dw);
                let child_x = le + container_child_x(&state.children, c_idx, extra_each);
                let (_pl, _pr, _extra) = container_child_slot(child, extra_each);
                return Some((child_x + _pl, child.min_width + _extra));
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
}

// ── Container child spacing helpers ────────────────────────────

fn container_total_width(children: &[Widget]) -> f64 {
    children.iter()
        .map(|c| {
            let pl = c.padding_left.map(|p| p as f64).unwrap_or(0.0);
            let pr = c.padding_right.map(|p| p as f64).unwrap_or(0.0);
            c.min_width + pl + pr
        })
        .sum()
}

fn container_auto_count(children: &[Widget]) -> usize {
    children.iter().filter(|c| c.padding_left.is_none()).count()
}

fn container_extra_each(children: &[Widget], available: f64) -> f64 {
    let total = container_total_width(children);
    let auto = container_auto_count(children);
    if available > total && auto > 0 {
        (available - total) / auto as f64
    } else {
        0.0
    }
}

fn container_child_slot(child: &Widget, extra_each: f64) -> (f64, f64, f64) {
    let pl = child.padding_left.map(|p| p as f64).unwrap_or(0.0);
    let pr = child.padding_right.map(|p| p as f64).unwrap_or(0.0);
    let extra = if child.padding_left.is_none() { extra_each } else { 0.0 };
    (pl, pr, extra)
}

fn container_child_x(children: &[Widget], idx: usize, extra_each: f64) -> f64 {
    children[..idx].iter().map(|c| {
        let (pl, pr, extra) = container_child_slot(c, extra_each);
        c.min_width + pl + pr + extra
    }).sum()
}

impl FunctionLayer {
    pub fn hit(&self, width: u16, height: u16, x: f64, y: f64, i: Option<usize>) -> Option<(usize, Option<usize>)> {
        if let Some(idx) = i {
            return self.widgets.get(idx).and_then(|(_, w)| {
                if y >= 0.1 * height as f64 && y <= 0.9 * height as f64 {
                    if let WidgetKind::Container(ref state) = w.kind {
                        let available = self.widget_position(width, idx).map(|(_, dw)| dw).unwrap_or_else(|| container_total_width(&state.children));
                        let extra_each = container_extra_each(&state.children, available);
                        let mut child_x = 0.0;
                        for (ci, child) in state.children.iter().enumerate() {
                            let (_pl, _pr, _extra) = container_child_slot(child, extra_each);
                            let child_w = child.min_width + _pl + _pr + _extra;
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
                if let WidgetKind::Container(ref state) = self.widgets[i].1.kind {
                    let extra_each = container_extra_each(&state.children, drawn_width);
                    let rel_x = x - left_edge;
                    let mut child_x = 0.0;
                    for (ci, child) in state.children.iter().enumerate() {
                        let (_pl, _pr, _extra) = container_child_slot(child, extra_each);
                        let child_w = child.min_width + _pl + _pr + _extra;
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
