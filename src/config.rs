// SPDX-License-Identifier: GPL-3.0-only
use cairo::FontFace;
use freetype::Library as FtLibrary;
use input_linux::Key;
use fontconfig::Fontconfig;
use nix::{
    errno::Errno,
    sys::inotify::{AddWatchFlags, InitFlags, Inotify, InotifyEvent, WatchDescriptor},
};
use serde::de;
use serde::{Deserialize, Deserializer};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs::read_to_string, os::fd::AsFd, collections::HashMap};

pub static DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum OutlineColor {
    Grayscale(f64),
    Rgb([f64; 3]),
}

impl OutlineColor {
    pub fn set_cairo_source(&self, c: &cairo::Context) {
        match self {
            OutlineColor::Grayscale(gray) => c.set_source_rgb(*gray, *gray, *gray),
            OutlineColor::Rgb([r, g, b]) => c.set_source_rgb(*r, *g, *b),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaddingSpec {
    Value(u16),
    Default,
}

impl<'de> Deserialize<'de> for PaddingSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct PaddingSpecVisitor;
        impl<'de> de::Visitor<'de> for PaddingSpecVisitor {
            type Value = PaddingSpec;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an integer or the string \"default\"")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<PaddingSpec, E> {
                Ok(PaddingSpec::Value(v as u16))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<PaddingSpec, E> {
                Ok(PaddingSpec::Value(v as u16))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<PaddingSpec, E> {
                if v.eq_ignore_ascii_case("default") {
                    Ok(PaddingSpec::Default)
                } else {
                    Err(de::Error::invalid_value(de::Unexpected::Str(v), &self))
                }
            }
        }
        deserializer.deserialize_any(PaddingSpecVisitor)
    }
}

// ── Common fields shared by all widget types ────────────────────

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CommonFields {
    pub show_outline: Option<bool>,
    pub outline_color: Option<OutlineColor>,
    pub active_color: Option<OutlineColor>,
    pub padding: Option<PaddingSpec>,
    pub padding_left: Option<PaddingSpec>,
    pub padding_right: Option<PaddingSpec>,
    pub margin: Option<u16>,
    pub margin_left: Option<u16>,
    pub margin_right: Option<u16>,
    pub font_size: Option<f64>,
    pub icon_size: Option<f64>,
    pub theme: Option<String>,
    pub slider_track_outline: Option<OutlineColor>,
}

// ── Slider type ─────────────────────────────────────────────────

#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum SliderType {
    Volume,
    Brightness,
    KeyboardBrightness,
}

impl SliderType {
    pub fn display_name(self) -> &'static str {
        match self {
            SliderType::Volume => "Volume",
            SliderType::Brightness => "Brightness",
            SliderType::KeyboardBrightness => "Kbd Brightness",
        }
    }
}

// ── Tagged widget config ────────────────────────────────────────

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "widget", rename_all = "snake_case")]
pub enum WidgetConfig {
    Button {
        icon: Option<String>,
        text: Option<String>,
        action: Option<crate::widget::Action>,
        #[serde(flatten)]
        common: CommonFields,
    },
    Slider {
        slider: SliderType,
        icon: Option<String>,
        text: Option<String>,
        handle_icon: Option<String>,
        bar_icon: Option<String>,
        handle_size: Option<u32>,
        live_update: Option<bool>,
        #[serde(flatten)]
        common: CommonFields,
    },
    Battery {
        mode: String,
        #[serde(flatten)]
        common: CommonFields,
    },
    Clock {
        format: String,
        locale: Option<String>,
        #[serde(flatten)]
        common: CommonFields,
    },
    Dynamic {
        #[serde(flatten)]
        common: CommonFields,
    },
    Container {
        widgets: Vec<WidgetConfig>,
        #[serde(flatten)]
        common: CommonFields,
    },
}

pub struct Config {
    pub show_outline: bool,
    pub enable_pixel_shift: bool,
    pub font_face: FontFace,
    pub font_size: f64,
    pub icon_size: f64,
    pub adaptive_brightness: bool,
    pub active_brightness: u32,
    pub brightness_dim_timeout_ms: u32,
    pub brightness_off_timeout_ms: u32,
    pub dimmed_brightness: u32,
    pub panel_timeout_seconds: u32,
    pub icon_theme: String,
    pub default_padding: Option<u16>,
    pub slider_color_source: String,
    pub slider_live_update: bool,
    pub outline_color: Option<OutlineColor>,
    pub active_color: Option<OutlineColor>,
    pub slider_track_outline: Option<OutlineColor>,
    pub main_layer: Vec<WidgetConfig>,
    pub fn_layer: Vec<WidgetConfig>,
    pub panels: HashMap<String, Vec<WidgetConfig>>,
    pub app_rules: HashMap<String, Vec<WidgetConfig>>,
}

fn load_font(name: &str) -> FontFace {
    let fc = Fontconfig::new().unwrap_or_else(|| {
        panic!("Unable to initialise fontconfig. If you are using the default config, make sure you have at least one font installed")
    });
    let (family, style) = name.split_once(':').unwrap_or((name, ""));
    let style = if style.is_empty() { None } else { Some(style) };
    let font = fc.find(family, style).unwrap_or_else(|| {
        panic!("Unable to find specified font '{}'. If you are using the default config, make sure you have at least one font installed", name)
    });
    let ft_library = FtLibrary::init().unwrap();
    let face = ft_library.new_face(&font.path.to_string_lossy().into_owned(), font.index.unwrap_or(0) as isize).unwrap();
    FontFace::create_from_ft(&face).unwrap()
}

// ── TOML proxy types ────────────────────────────────────────────

#[derive(Deserialize)]
struct ConfigFile {
    global: Option<GlobalProxy>,
    layers: Option<LayersProxy>,
    panels: Option<HashMap<String, PanelProxy>>,
    app_rules: Option<HashMap<String, PanelProxy>>,
}

#[derive(Deserialize, Clone, Default)]
struct GlobalProxy {
    debug: Option<bool>,
    show_outline: Option<bool>,
    enable_pixel_shift: Option<bool>,
    font_template: Option<String>,
    font_size: Option<f64>,
    icon_size: Option<f64>,
    adaptive_brightness: Option<bool>,
    active_brightness: Option<u32>,
    brightness_dim_timeout_ms: Option<u32>,
    brightness_off_timeout_ms: Option<u32>,
    dimmed_brightness: Option<u32>,
    panel_timeout_seconds: Option<u32>,
    icon_theme: Option<String>,
    default_padding: Option<u16>,
    slider_color_source: Option<String>,
    slider_live_update: Option<bool>,
    outline_color: Option<OutlineColor>,
    active_color: Option<OutlineColor>,
    slider_track_outline: Option<OutlineColor>,
}

#[derive(Deserialize)]
struct LayersProxy {
    main: Option<PanelProxy>,
    #[serde(rename = "fn")]
    fn_layer: Option<PanelProxy>,
}

#[derive(Deserialize)]
struct PanelProxy {
    widgets: Option<Vec<WidgetConfig>>,
}

// ── Merge helpers ───────────────────────────────────────────────

macro_rules! merge_if_some {
    ($base:expr, $user:expr, $($field:ident),+ $(,)?) => {
        $(
            if let Some(v) = $user.$field.take() {
                $base.$field = Some(v);
            }
        )+
    };
}

fn merge_global(base: &mut GlobalProxy, mut user: GlobalProxy) {
    merge_if_some!(base, &mut user,
        debug, show_outline, enable_pixel_shift, font_template, font_size,
        icon_size, adaptive_brightness, active_brightness,
        brightness_dim_timeout_ms, brightness_off_timeout_ms,
        dimmed_brightness, panel_timeout_seconds, icon_theme,
        default_padding, slider_color_source, slider_live_update,
        outline_color, active_color, slider_track_outline,
    );
}

fn load_toml(path: &str) -> Option<ConfigFile> {
    match read_to_string(path) {
        Ok(content) => match toml::from_str(&content) {
            Ok(cf) => Some(cf),
            Err(e) => {
                eprintln!("Warning: failed to parse {}: {}", path, e);
                None
            }
        },
        Err(e) => {
            eprintln!("Warning: could not read {}: {}", path, e);
            None
        }
    }
}

fn load_toml_user(filename: &str) -> Option<ConfigFile> {
    let path = crate::paths::user_config_dir().join("configs").join(filename);
    match std::fs::read_to_string(&path).ok() {
        Some(content) => match toml::from_str(&content) {
            Ok(cf) => {
                println!("ktouchbar: loaded user config from {}", path.display());
                Some(cf)
            }
            Err(e) => {
                eprintln!("Warning: failed to parse user config at {}: {}", path.display(), e);
                None
            }
        },
        None => {
            eprintln!("Warning: could not read user config at {} (not found or unreadable)", path.display());
            None
        }
    }
}

// ── Exposed loading ─────────────────────────────────────────────

pub fn load_config() -> Config {
    let sys = load_toml("/usr/share/ktouchbar/configs/config.toml");
    let user = load_toml_user("config.toml");

    let mut global = GlobalProxy::default();
    if let Some(ref cf) = sys {
        if let Some(ref g) = cf.global {
            merge_global(&mut global, GlobalProxy {
                debug: g.debug,
                show_outline: g.show_outline,
                enable_pixel_shift: g.enable_pixel_shift,
                font_template: g.font_template.clone(),
                font_size: g.font_size,
                icon_size: g.icon_size,
                adaptive_brightness: g.adaptive_brightness,
                active_brightness: g.active_brightness,
                brightness_dim_timeout_ms: g.brightness_dim_timeout_ms,
                brightness_off_timeout_ms: g.brightness_off_timeout_ms,
                dimmed_brightness: g.dimmed_brightness,
                panel_timeout_seconds: g.panel_timeout_seconds,
                icon_theme: g.icon_theme.clone(),
                default_padding: g.default_padding,
                slider_color_source: g.slider_color_source.clone(),
                slider_live_update: g.slider_live_update,
                outline_color: g.outline_color.clone(),
                active_color: g.active_color.clone(),
                slider_track_outline: g.slider_track_outline.clone(),
            });
        }
    }
    if let Some(ref cf) = user {
        if let Some(ref g) = cf.global {
            merge_global(&mut global, g.clone());
        }
    }

    let cfg_debug = global.debug.unwrap_or(false);
    DEBUG_ENABLED.store(cfg_debug, Ordering::Relaxed);
    let icon_theme = global.icon_theme.clone().unwrap_or_else(|| "auto".to_string());
    let default_padding = global.default_padding;

    let mut main_widgets: Vec<WidgetConfig> = Vec::new();
    let mut fn_widgets: Vec<WidgetConfig> = Vec::new();

    if let Some(ref cf) = sys {
        if let Some(ref l) = cf.layers {
            if let Some(ref m) = l.main {
                if let Some(ref w) = m.widgets { main_widgets = w.clone(); }
            }
            if let Some(ref f) = l.fn_layer {
                if let Some(ref w) = f.widgets { fn_widgets = w.clone(); }
            }
        }
    }
    if let Some(ref cf) = user {
        if let Some(ref l) = cf.layers {
            if let Some(ref m) = l.main {
                if let Some(ref w) = m.widgets { main_widgets = w.clone(); }
            }
            if let Some(ref f) = l.fn_layer {
                if let Some(ref w) = f.widgets { fn_widgets = w.clone(); }
            }
        }
    }

    if main_widgets.is_empty() {
        main_widgets = (1..=12).map(|i| {
            let key = match i {
                1 => vec![Key::F1], 2 => vec![Key::F2], 3 => vec![Key::F3],
                4 => vec![Key::F4], 5 => vec![Key::F5], 6 => vec![Key::F6],
                7 => vec![Key::F7], 8 => vec![Key::F8], 9 => vec![Key::F9],
                10 => vec![Key::F10], 11 => vec![Key::F11],
                _ => vec![Key::F12],
            };
            WidgetConfig::Button {
                icon: None,
                text: Some(format!("F{i}")),
                action: Some(crate::widget::Action::Key(key)),
                common: CommonFields::default(),
            }
        }).collect();
    }
    if fn_widgets.is_empty() {
        fn_widgets = vec![
            WidgetConfig::Battery {
                mode: "both".to_string(),
                common: CommonFields {
                    show_outline: Some(false),
                    padding: Some(PaddingSpec::Default),
                    icon_size: Some(32.0),
                    ..Default::default()
                },
            },
            WidgetConfig::Clock {
                format: "%I:%M%P %-e/%m".to_string(),
                locale: None,
                common: CommonFields {
                    show_outline: Some(false),
                    outline_color: Some(OutlineColor::Grayscale(0.2)),
                    padding: Some(PaddingSpec::Default),
                    ..Default::default()
                },
            },
            WidgetConfig::Dynamic {
                common: CommonFields {
                    show_outline: Some(false),
                    ..Default::default()
                },
            },
        ];
    }

    let mut panels: HashMap<String, Vec<WidgetConfig>> = HashMap::new();
    if let Some(ref cf) = sys {
        if let Some(ref p) = cf.panels {
            for (name, panel) in p {
                if let Some(ref w) = panel.widgets {
                    panels.insert(name.clone(), w.clone());
                }
            }
        }
    }
    if let Some(ref cf) = user {
        if let Some(ref p) = cf.panels {
            for (name, panel) in p {
                if let Some(ref w) = panel.widgets {
                    panels.insert(name.clone(), w.clone());
                }
            }
        }
    }

    let mut app_rules: HashMap<String, Vec<WidgetConfig>> = HashMap::new();
    if let Some(ref cf) = sys {
        if let Some(ref a) = cf.app_rules {
            for (class, rule) in a {
                if let Some(ref w) = rule.widgets {
                    app_rules.insert(class.clone(), w.clone());
                }
            }
        }
    }
    if let Some(ref cf) = user {
        if let Some(ref a) = cf.app_rules {
            for (class, rule) in a {
                if let Some(ref w) = rule.widgets {
                    app_rules.insert(class.clone(), w.clone());
                }
            }
        }
    }

    Config {
        show_outline: global.show_outline.unwrap_or(true),
        enable_pixel_shift: global.enable_pixel_shift.unwrap_or(false),
        font_face: load_font(global.font_template.as_deref().unwrap_or("sans-serif")),
        font_size: global.font_size.unwrap_or(32.0),
        icon_size: global.icon_size.unwrap_or(48.0),
        adaptive_brightness: global.adaptive_brightness.unwrap_or(true),
        active_brightness: global.active_brightness.unwrap_or(128),
        brightness_dim_timeout_ms: global.brightness_dim_timeout_ms.unwrap_or(120000),
        brightness_off_timeout_ms: global.brightness_off_timeout_ms.unwrap_or(300000),
        dimmed_brightness: global.dimmed_brightness.unwrap_or(1),
        panel_timeout_seconds: global.panel_timeout_seconds.unwrap_or(5),
        icon_theme: icon_theme.clone(),
        default_padding,
        slider_color_source: global.slider_color_source.clone().unwrap_or_else(|| "kde".into()),
        slider_live_update: global.slider_live_update.unwrap_or(false),
        outline_color: global.outline_color,
        active_color: global.active_color,
        slider_track_outline: global.slider_track_outline,
        main_layer: main_widgets,
        fn_layer: fn_widgets,
        panels,
        app_rules,
    }
}

// ── Config defaults (for system daemon — no file I/O) ───────────

impl Config {
    pub fn fallback_defaults() -> Config {
        Config {
            show_outline: true,
            enable_pixel_shift: false,
            font_face: load_font("sans-serif"),
            font_size: 32.0,
            icon_size: 48.0,
            adaptive_brightness: false,
            active_brightness: 255,
            brightness_dim_timeout_ms: u32::MAX,
            brightness_off_timeout_ms: u32::MAX,
            dimmed_brightness: 1,
            panel_timeout_seconds: 5,
            icon_theme: "auto".to_string(),
            default_padding: None,
            slider_color_source: "kde".to_string(),
            slider_live_update: false,
            outline_color: None,
            active_color: None,
            slider_track_outline: None,
            main_layer: vec![],
            fn_layer: vec![],
            panels: HashMap::new(),
            app_rules: HashMap::new(),
        }
    }
}

// ── Hot-reload via inotify ──────────────────────────────────────

pub struct ConfigManager {
    inotify_fd: Inotify,
    watch_desc: Option<WatchDescriptor>,
}

fn arm_inotify(inotify_fd: &Inotify) -> Option<WatchDescriptor> {
    let flags = AddWatchFlags::IN_MOVED_TO | AddWatchFlags::IN_CLOSE_WRITE | AddWatchFlags::IN_ONESHOT;
    let config_dir = crate::paths::user_config_dir().join("configs");
    match inotify_fd.add_watch(&config_dir, flags) {
        Ok(wd) => Some(wd),
        Err(Errno::ENOENT) => None,
        Err(e) => {
            eprintln!("ktouchbar: inotify add_watch error: {}", e);
            None
        }
    }
}

impl Default for ConfigManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigManager {
    pub fn new() -> ConfigManager {
        let inotify_fd = Inotify::init(InitFlags::IN_NONBLOCK).unwrap();
        let watch_desc = arm_inotify(&inotify_fd);
        ConfigManager { inotify_fd, watch_desc }
    }

    pub fn load_config(&self) -> Config {
        load_config()
    }

    pub fn update_config(&mut self, cfg: &mut Config) -> bool {
        if self.watch_desc.is_none() {
            self.watch_desc = arm_inotify(&self.inotify_fd);
            return false;
        }
        match self.inotify_fd.read_events() {
            Err(Errno::EAGAIN) => false,
            r => self.handle_events(cfg, r),
        }
    }

    #[cold]
    fn handle_events(
        &mut self,
        cfg: &mut Config,
        evts: Result<Vec<InotifyEvent>, Errno>,
    ) -> bool {
        let mut ret = false;
        for evt in evts.unwrap() {
            if Some(evt.wd) != self.watch_desc {
                continue;
            }
            *cfg = load_config();
            ret = true;
            self.watch_desc = arm_inotify(&self.inotify_fd);
        }
        ret
    }

    pub fn fd(&self) -> &impl AsFd {
        &self.inotify_fd
    }
}
