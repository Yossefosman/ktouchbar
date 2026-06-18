// SPDX-License-Identifier: GPL-3.0-only
use anyhow::{anyhow, Result};
use cairo::{Antialias, Format, ImageSurface};
use freedesktop::{IconTheme, Pixmap, xdg_data_home};
use librsvg_rebind::prelude::HandleExt;
use librsvg_rebind::{Handle, Rectangle};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::fs::File;

const ICON_EXTENSIONS: &[&str] = &["svg", "png", "xpm"];
const ICON_SIZE: i32 = 48;

// ── Icon theme lookup ────────────────────────────────────────────

pub(crate) fn get_icon_flexible(name: &str, theme: Option<&str>) -> Option<PathBuf> {
    let icon_theme = match theme {
        None | Some("auto") => IconTheme::current(),
        Some(t) => IconTheme::from_name(t)?,
    };
    get_icon_from_theme(&icon_theme, name)
}

fn get_icon_from_theme(theme: &IconTheme, name: &str) -> Option<PathBuf> {
    let dirs = theme.config_value("Icon Theme", "Directories")?;
    let base = theme.path();
    let overlay = xdg_data_home().join("icons").join(theme.name());
    for dir_name in dirs.split(',') {
        for ext in ICON_EXTENSIONS {
            let filename = format!("{name}.{ext}");
            let overlay_path = overlay.join(dir_name).join(&filename);
            if overlay_path.exists() {
                return Some(overlay_path);
            }
            let theme_path = base.join(dir_name).join(&filename);
            if theme_path.exists() {
                return Some(theme_path);
            }
        }
    }
    for inherited in theme.inherits() {
        if let Some(t) = IconTheme::from_name(&inherited) {
            if let Some(p) = get_icon_from_theme(&t, name) {
                return Some(p);
            }
        }
    }
    Pixmap::get(name)
}

// ── Icon cache ───────────────────────────────────────────────────

fn cache_key(name: &str, theme: Option<&str>) -> String {
    match theme {
        Some(t) => format!("{t}:{name}"),
        None => format!(":{name}"),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct IconCacheEntry {
    pub path: PathBuf,
    pub last_used: std::time::Instant,
}

pub struct IconLoader {
    cache: Arc<Mutex<HashMap<String, IconCacheEntry>>>,
}

impl Default for IconLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl IconLoader {
    pub fn new() -> Self {
        IconLoader { cache: Arc::new(Mutex::new(HashMap::new())) }
    }

    fn find_icon_path(name: &str, theme: Option<&str>) -> Option<PathBuf> {
        get_icon_flexible(name, theme)
    }

    pub fn load_async(&self, name: String, theme: Option<String>) -> std::sync::mpsc::Receiver<Option<PathBuf>> {
        let key = cache_key(&name, theme.as_deref());
        {
            let cache = self.cache.lock().unwrap();
            if let Some(entry) = cache.get(&key) {
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(Some(entry.path.clone())).unwrap();
                return rx;
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let cache = self.cache.clone();
        thread::spawn(move || {
            if let Some(ref p) = IconLoader::find_icon_path(&name, theme.as_deref()) {
                cache.lock().unwrap().insert(
                    key,
                    IconCacheEntry { path: p.clone(), last_used: std::time::Instant::now() },
                );
                let _ = tx.send(Some(p.clone()));
            } else {
                let _ = tx.send(None);
            }
        });
        rx
    }

    pub fn cleanup(&self) {
        let mut cache = self.cache.lock().unwrap();
        let now = std::time::Instant::now();
        cache.retain(|_, entry| now.duration_since(entry.last_used) < Duration::from_secs(300));
    }
}

pub static ICON_LOADER: std::sync::LazyLock<IconLoader> = std::sync::LazyLock::new(IconLoader::new);

pub(crate) fn get_icon_cached(name: String, theme: Option<String>) -> Option<PathBuf> {
    let key = cache_key(&name, theme.as_deref());
    if let Some(entry) = ICON_LOADER.cache.lock().unwrap().get(&key) {
        return Some(entry.path.clone());
    }
    if let Some(path) = IconLoader::find_icon_path(&name, theme.as_deref()) {
        ICON_LOADER.cache.lock().unwrap().insert(
            key,
            IconCacheEntry { path: path.clone(), last_used: std::time::Instant::now() },
        );
        return Some(path);
    }
    None
}

pub fn cleanup_cache() {
    ICON_LOADER.cleanup();
}

pub fn preload_common_icons() {
    const COMMON_ICONS: &[(&str, Option<&str>)] = &[
        ("back", Some("breeze")),
        ("settings", Some("breeze")),
        ("application-default-icon", None),
        ("dynamic-shortcuts", None),
        ("bolt", Some("breeze")),
        ("brightness-low", Some("breeze")),
        ("brightness-high", Some("breeze")),
        ("volume-up", Some("breeze")),
        ("volume-down", Some("breeze")),
        ("media-playback-pause", Some("breeze")),
    ];
    for &(name, theme) in COMMON_ICONS {
        let _ = ICON_LOADER.load_async(name.to_string(), theme.map(|s| s.to_string()));
    }
    preload_app_icons();
}

fn preload_app_icons() {
    const COMMON_APP_CLASSES: &[&str] = &[
        "code", "Code", "VSCode",
        "firefox", "Firefox",
        "chromium", "Chromium", "chrome", "Chrome",
        "alacritty", "Alacritty",
        "terminal", "Terminal", "gnome-terminal",
        "nautilus", "Nautilus", "Files",
        "discord", "Discord",
        "spotify", "Spotify",
        "steam", "Steam",
        "obs", "OBS",
        "gimp", "GIMP",
        "inkscape", "Inkscape",
        "blender", "Blender",
        "thunderbird", "Thunderbird",
        "libreoffice", "LibreOffice",
        "vlc", "VLC",
    ];
    for &class in COMMON_APP_CLASSES {
        let lower = class.to_lowercase();
        let _ = ICON_LOADER.load_async(lower.clone(), Some("breeze".to_string()));
        let _ = ICON_LOADER.load_async(format!("app-{}", lower), None);
    }
}

pub fn start_background_preloader() {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(5));
        preload_extended_icons();
    });
}

fn preload_extended_icons() {
    const EXTENDED_ICONS: &[(&str, Option<&str>)] = &[
        ("mic-off", Some("breeze")),
        ("input-keyboard-brightness", Some("breeze")),
        ("media-skip-backward", Some("breeze")),
        ("media-skip-forward", Some("breeze")),
        ("applications-other", Some("breeze")),
        ("utilities-terminal", Some("breeze")),
        ("color-picker", Some("breeze")),
        ("system-run", Some("breeze")),
        ("camera-record", Some("breeze")),
        ("camera-photo", Some("breeze")),
    ];
    for &(name, theme) in EXTENDED_ICONS {
        let _ = ICON_LOADER.load_async(name.to_string(), theme.map(|s| s.to_string()));
    }
}

// ── Image loading ────────────────────────────────────────────────

pub(crate) fn try_load_svg(path: &str) -> Result<Handle> {
    Handle::from_file(path)?.ok_or(anyhow!("failed to load image"))
}

pub(crate) fn try_load_png(path: impl AsRef<Path>) -> Result<ImageSurface> {
    load_image_surface_at(path.as_ref(), ICON_SIZE as f64)
}

/// Load SVG or PNG from a file path, rendering at the requested size.
pub(crate) fn load_image_surface(path: &Path, size: f64) -> Result<ImageSurface> {
    load_image_surface_at(path, size)
}

fn load_image_surface_at(path: &Path, size: f64) -> Result<ImageSurface> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    match ext {
        "svg" => {
            let handle = try_load_svg(path.to_str().ok_or(anyhow!("path not utf-8"))?)?;
            let s = size.ceil() as i32;
            let surface = ImageSurface::create(Format::ARgb32, s, s)?;
            let c = cairo::Context::new(&surface)?;
            handle.render_document(&c, &Rectangle::new(0.0, 0.0, size, size))?;
            Ok(surface)
        }
        "png" => {
            let mut file = File::open(path)?;
            let surf = ImageSurface::create_from_png(&mut file)?;
            let s = size.ceil() as i32;
            if surf.width() == s && surf.height() == s {
                return Ok(surf);
            }
            let resized = ImageSurface::create(Format::ARgb32, s, s)?;
            let c = cairo::Context::new(&resized)?;
            c.scale(s as f64 / surf.width() as f64, s as f64 / surf.height() as f64);
            c.set_source_surface(&surf, 0.0, 0.0)?;
            c.set_antialias(Antialias::Best);
            c.paint()?;
            Ok(resized)
        }
        _ => Err(anyhow!("unsupported image format")),
    }
}

pub fn try_load_image(
    name: impl AsRef<str>,
    theme: Option<impl AsRef<str>>,
) -> Result<(Handle, Option<ImageSurface>)> {
    let name = name.as_ref();
    let theme_str = theme.as_ref().map(|s| s.as_ref());
    if let Some(cached_path) =
        get_icon_cached(name.to_string(), theme_str.map(|s| s.to_string()))
    {
        return match cached_path.extension().and_then(|s| s.to_str()) {
            Some("png") => try_load_png(&cached_path).map(|s| (Handle::from_file("").unwrap().unwrap(), Some(s))),
            Some("svg") => try_load_svg(cached_path.to_str().unwrap_or("")).map(|h| (h, None)),
            _ => Err(anyhow!("Unsupported file format")),
        };
    }
    try_load_image_sync(name, theme_str)
}

fn try_load_image_sync(name: &str, theme: Option<&str>) -> Result<(Handle, Option<ImageSurface>)> {
    let icon_path = get_icon_flexible(name, theme);
    match icon_path {
        Some(path) => {
            let result = match path.extension().and_then(|s| s.to_str()) {
                Some("png") => try_load_png(&path).map(|s| (Handle::from_file("").unwrap().unwrap(), Some(s))),
                Some("svg") => try_load_svg(
                    path.to_str().ok_or(anyhow!("image path is not unicode"))?,
                ).map(|h| (h, None)),
                _ => Err(anyhow!("invalid file extension")),
            };
            result
        }
        None => Err(anyhow!("icon {name} not found in theme {theme:?}")),
    }
}

pub fn make_placeholder_icon() -> ImageSurface {
    let surface = ImageSurface::create(Format::ARgb32, ICON_SIZE, ICON_SIZE).unwrap();
    let c = cairo::Context::new(&surface).unwrap();
    let s = ICON_SIZE as f64;
    c.set_source_rgba(0.0, 0.0, 0.0, 0.0);
    c.paint().unwrap();
    let inset = 3.0;
    c.set_source_rgb(0.35, 0.35, 0.35);
    c.set_line_width(1.5);
    c.rectangle(inset, inset, s - inset * 2.0, s - inset * 2.0);
    c.stroke().unwrap();
    c.set_source_rgb(0.55, 0.55, 0.55);
    c.select_font_face("sans-serif", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    c.set_font_size(14.0);
    let label = "?";
    let extents = c.text_extents(label).unwrap();
    c.move_to(
        (s - extents.width()) / 2.0 - extents.x_bearing(),
        (s - extents.height()) / 2.0 - extents.y_bearing(),
    );
    c.show_text(label).unwrap();
    surface
}

/// Returns a Handle for the battery icon, or a temp-generated SVG file.
pub fn load_battery_image(icon: &str, theme: Option<impl AsRef<str>>) -> Handle {
    if let Ok((handle, _)) = try_load_image(icon, theme) {
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
