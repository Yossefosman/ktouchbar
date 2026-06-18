// SPDX-License-Identifier: GPL-3.0-only
use fontconfig::Fontconfig;

pub fn find_font(template: &str) -> Option<(String, i32)> {
    let fc = Fontconfig::new()?;
    let (family, style) = template.split_once(':').unwrap_or((template, ""));
    let style = if style.is_empty() { None } else { Some(style) };
    let font = fc.find(family, style)?;
    Some((font.path.to_string_lossy().into_owned(), font.index.unwrap_or(0)))
}
