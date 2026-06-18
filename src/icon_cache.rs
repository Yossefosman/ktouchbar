// SPDX-License-Identifier: GPL-3.0-only
use freedesktop::{IconTheme, Pixmap, xdg_data_home};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const ICON_EXTENSIONS: &[&str] = &["svg", "png", "xpm"];

pub fn get_icon_flexible(name: &str, theme: Option<&str>) -> Option<PathBuf> {
    let icon_theme = match theme {
        None | Some("auto") => IconTheme::current(),
        Some(t) => IconTheme::from_name(t)?,
    };
    get_icon_from_theme(&icon_theme, name)
}

fn get_icon_from_theme(theme: &IconTheme, name: &str) -> Option<PathBuf> {
    let dirs = theme.config_value("Icon Theme", "Directories")?;
    let base = theme.path();
    let overlay = xdg_data_home()
        .join("icons")
        .join(theme.name());

    for dir_name in dirs.split(',') {
        for ext in ICON_EXTENSIONS {
            let filename = format!("{name}.{ext}");
            // Check overlay first (user local overrides)
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

fn cache_key(name: &str, theme: Option<&str>) -> String {
    match theme {
        Some(t) => format!("{t}:{name}"),
        None => format!(":{name}"),
    }
}

#[derive(Debug, Clone)]
pub struct IconCacheEntry {
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
        IconLoader {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
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
                    IconCacheEntry {
                        path: p.clone(),
                        last_used: std::time::Instant::now(),
                    },
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

pub fn get_icon_cached(name: String, theme: Option<String>) -> Option<PathBuf> {
    let key = cache_key(&name, theme.as_deref());
    if let Some(entry) = ICON_LOADER.cache.lock().unwrap().get(&key) {
        return Some(entry.path.clone());
    }

    if let Some(path) = IconLoader::find_icon_path(&name, theme.as_deref()) {
        ICON_LOADER.cache.lock().unwrap().insert(
            key,
            IconCacheEntry {
                path: path.clone(),
                last_used: std::time::Instant::now(),
            },
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

pub fn preload_app_icons() {
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
