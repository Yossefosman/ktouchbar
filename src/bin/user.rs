// SPDX-License-Identifier: GPL-3.0-only
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Seek, SeekFrom, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use cairo::{Format, ImageSurface};
use chrono::{Local, Timelike};
use nix::sys::signal::{self, SigAction, SigHandler, SigSet, Signal};
use zbus::zvariant;

static SIGTERM_RECEIVED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigterm(_: i32) {
    SIGTERM_RECEIVED.store(true, Ordering::SeqCst);
}

use ktouchbar::config::{Config, ConfigManager, WidgetConfig};
use ktouchbar::display::pixel_shift::PixelShiftManager;
use ktouchbar::widget::{self, dynamicshortcuts, icon, FunctionLayer, WidgetConfigCtx};
use ktouchbar::widget::Action;

const DE_POLL_MS: i32 = 50;
const HW_POLL_MS: i32 = 30;
const NAME_LIFETIME: &str = "org.ktouchbar.UserInterface";

#[derive(Clone, Debug, serde::Deserialize, zvariant::Type)]
struct TouchEventData {
    slot: u32,
    x: f64,
    y: f64,
    event_type: u32,
}

fn expand_dynamic_widgets(
    widget_cfgs: &[WidgetConfig],
    app_rules: &HashMap<String, Vec<WidgetConfig>>,
) -> Vec<WidgetConfig> {
    use ktouchbar::config::CommonFields;
    let window_class = dynamicshortcuts::get_active_window_info()
        .ok()
        .map(|info| info.class);
    let mut out = Vec::new();
    for wc in widget_cfgs {
        if let WidgetConfig::Dynamic { common } = wc {
            if let Some(ref class) = window_class {
                let children = if let Some(rules) = app_rules.get(class) {
                    rules.clone()
                } else {
                    vec![WidgetConfig::Button {
                        icon: None, text: Some(class.clone()),
                        action: None,
                        common: CommonFields::default(),
                    }]
                };
                out.push(WidgetConfig::Container {
                    widgets: children,
                    common: common.clone(),
                });
            }
        } else {
            out.push(wc.clone());
        }
    }
    out
}

fn build_layer(
    widget_cfgs: &[WidgetConfig],
    config: &Config,
) -> FunctionLayer {
    let expanded = expand_dynamic_widgets(widget_cfgs, &config.app_rules);
    let ctx = WidgetConfigCtx {
        global_theme: Some(&config.icon_theme),
        default_padding: config.default_padding,
        slider_color_source: Some(&config.slider_color_source),
        global_outline_color: config.outline_color.as_ref(),
        global_active_color: config.active_color.as_ref(),
        global_slider_live_update: Some(config.slider_live_update),
        global_track_outline: config.slider_track_outline.as_ref(),
    };
    if expanded.is_empty() {
        return FunctionLayer::with_config(
            vec![WidgetConfig::Button {
                icon: None, text: Some("ktouchbar".to_string()),
                action: None,
                common: ktouchbar::config::CommonFields::default(),
            }],
            &ctx,
        );
    }
    FunctionLayer::with_config(expanded, &ctx)
}

fn rebuild_cached_layers(
    config: &Config,
) -> (FunctionLayer, FunctionLayer) {
    let main = build_layer(&config.main_layer, config);
    let fn_layer = build_layer(&config.fn_layer, config);
    (main, fn_layer)
}

fn import_graphical_env() {
    if let Ok(out) = std::process::Command::new("systemctl")
        .args(["--user", "show-environment"])
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((key, val)) = line.split_once('=') {
                if matches!(
                    key,
                    "DISPLAY" | "WAYLAND_DISPLAY" | "XAUTHORITY" | "XDG_SESSION_TYPE"
                ) {
                    std::env::set_var(key, val.trim());
                }
            }
        }
    }
}

// ── Desktop user detection ──────────────────────────────────────

fn detect_desktop_user() -> Option<String> {
    if let Ok(user) = std::env::var("SUDO_USER") {
        return Some(user);
    }

    if let Ok(output) = std::process::Command::new("loginctl")
        .args(["list-sessions", "--no-legend"])
        .output() {
        if let Ok(sessions) = String::from_utf8(output.stdout) {
            for line in sessions.lines() {
                let mut parts = line.split_whitespace();
                let session_id = parts.next();
                let _display = parts.next();
                let user_name = parts.next();
                let seat = parts.next();
                if let (Some(session_id), Some(user_name), Some(seat)) = (session_id, user_name, seat) {
                    if seat == "seat0" && user_name != "root" {
                        if let Ok(session_output) = std::process::Command::new("loginctl")
                            .args(["show-session", session_id, "-p", "Type"])
                            .output() {
                            if let Ok(session_info) = String::from_utf8(session_output.stdout) {
                                if session_info.contains("Type=wayland") || session_info.contains("Type=x11") {
                                    return Some(user_name.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Ok(entries) = std::fs::read_dir("/run/user") {
        for entry in entries.flatten() {
            if let Some(uid_str) = entry.file_name().to_str() {
                if let Ok(uid) = uid_str.parse::<u32>() {
                    if (1000..65534).contains(&uid) {
                        let wayland_socket = entry.path().join("wayland-0");
                        if wayland_socket.exists() {
                            if let Ok(output) = std::process::Command::new("getent")
                                .args(["passwd", uid_str])
                                .output() {
                                if let Ok(passwd_line) = String::from_utf8(output.stdout) {
                                    if let Some(username) = passwd_line.split(':').next() {
                                        return Some(username.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

// ── System monitor (minute tracker, cache cleanup ticker) ───────

use std::sync::Mutex;

#[derive(Debug, Clone)]
struct SystemState {
    current_minute: u32,
    cache_cleanup_due: bool,
}

impl SystemState {
    fn new() -> Self {
        let now = Local::now();
        SystemState {
            current_minute: now.minute(),
            cache_cleanup_due: false,
        }
    }
}

static SYSTEM_STATE: std::sync::LazyLock<Mutex<SystemState>> =
    std::sync::LazyLock::new(|| Mutex::new(SystemState::new()));

struct SystemMonitor {
    _handle: thread::JoinHandle<()>,
}

impl SystemMonitor {
    fn new() -> Self {
        let handle = thread::spawn(move || {
            Self::monitor_loop();
        });

        SystemMonitor { _handle: handle }
    }

    fn monitor_loop() {
        let mut cache_cleanup_counter = 0u32;

        loop {
            let current_time = Local::now();
            let current_minute = current_time.minute();

            if let Ok(mut state) = SYSTEM_STATE.lock() {
                if state.current_minute != current_minute {
                    state.current_minute = current_minute;
                }

                cache_cleanup_counter += 1;
                if cache_cleanup_counter >= 60 {
                    state.cache_cleanup_due = true;
                    cache_cleanup_counter = 0;
                }
            }

            thread::sleep(Duration::from_secs(5));
        }
    }
}

fn get_current_minute() -> u32 {
    if let Ok(state) = SYSTEM_STATE.lock() {
        state.current_minute
    } else {
        Local::now().minute()
    }
}

fn should_cleanup_cache() -> bool {
    if let Ok(mut state) = SYSTEM_STATE.lock() {
        if state.cache_cleanup_due {
            state.cache_cleanup_due = false;
            true
        } else {
            false
        }
    } else {
        false
    }
}

fn main() {
    let sig_action = SigAction::new(
        SigHandler::Handler(handle_sigterm),
        signal::SaFlags::empty(),
        SigSet::empty(),
    );
    unsafe { signal::sigaction(Signal::SIGTERM, &sig_action) }.unwrap();

    import_graphical_env();
    let sys_conn = match zbus::blocking::Connection::system() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ktouchbar-user: failed to connect to system bus: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = sys_conn.request_name(NAME_LIFETIME) {
        eprintln!("ktouchbar-user: failed to request {NAME_LIFETIME}: {e}");
        std::process::exit(1);
    }

    let (fb_width, fb_height, mode_hdisplay, mode_vdisplay): (u32, u32, u32, u32) = match sys_conn.call_method(
        Some("org.ktouchbar.Hardware"),
        "/org/ktouchbar/Hardware",
        Some("org.ktouchbar.Hardware"),
        "GetDisplayInfo",
        &(),
    ) {
        Ok(msg) => msg.body().unwrap_or((0, 0, 0, 0)),
        Err(e) => {
            eprintln!("ktouchbar-user: failed to call GetDisplayInfo: {e}");
            std::process::exit(1);
        }
    };

    if fb_width == 0 || fb_height == 0 || mode_hdisplay == 0 || mode_vdisplay == 0 {
        eprintln!("ktouchbar-user: invalid display dimensions from HW");
        std::process::exit(1);
    }

    println!("ktouchbar-user: fb {}x{}, mode {}x{}", fb_width, fb_height, mode_vdisplay, mode_hdisplay);

    detect_desktop_user().map(|u| dynamicshortcuts::init(&u));

    let mut cfg_mgr = ConfigManager::new();
    let mut cfg = cfg_mgr.load_config();
    let rebuild = rebuild_cached_layers(&cfg);
    let mut cached_main = rebuild.0;
    let mut cached_fn = rebuild.1;
    let mut layers = [cached_main.clone(), cached_fn.clone()];
    send_backlight_config(&sys_conn, &cfg);

    icon::preload_common_icons();

    let _battery_monitor = widget::battery::find_battery_device().map(widget::battery::BatteryMonitor::new);
    let _system_monitor = SystemMonitor::new();
    icon::start_background_preloader();

    let mut surface =
        ImageSurface::create(Format::ARgb32, fb_width as i32, fb_height as i32).unwrap();
    let mut active_layer = 0;
    let mut needs_complete_redraw = true;
    let mut navigation_state = NavigationState::new();
    let mut touches: HashMap<u32, (usize, usize, Option<usize>)> = HashMap::new();
    let mut pending_actions: Vec<PendingAction> = Vec::new();
    let mut held_keys: HashSet<u32> = HashSet::new();

    let mut pixel_shift = PixelShiftManager::new();
    let mut last_redraw_minute = Local::now().minute();
    let mut last_battery_update_minute = Local::now().minute();
    let mut last_hw_poll = Instant::now();

    loop {
        if cfg_mgr.update_config(&mut cfg) {
            let rebuild = rebuild_cached_layers(&cfg);
            cached_main = rebuild.0;
            cached_fn = rebuild.1;
            active_layer = 0;
            navigation_state.reset_to_main();
            layers[0] = cached_main.clone();
            layers[1] = cached_fn.clone();
            touches.clear();
            release_all_held_keys(&sys_conn, &mut held_keys);
            pending_actions.clear();
            needs_complete_redraw = true;
            send_backlight_config(&sys_conn, &cfg);
        }

        if navigation_state.current_panel.is_some()
            && navigation_state.should_timeout(cfg.panel_timeout_seconds)
        {
            navigation_state.reset_to_main();
            layers[0] = cached_main.clone();
            layers[1] = cached_fn.clone();
            needs_complete_redraw = true;
            clear_all_touches(&mut layers, &mut touches);
            release_all_held_keys(&sys_conn, &mut held_keys);
        }

        if last_hw_poll.elapsed() >= Duration::from_millis(HW_POLL_MS as u64) {
            last_hw_poll = Instant::now();
            let mut state = EventState {
                layers: &mut layers,
                navigation_state: &mut navigation_state,
                touches: &mut touches,
                held_keys: &mut held_keys,
                needs_complete_redraw: &mut needs_complete_redraw,
                active_layer: &mut active_layer,
                pending_actions: &mut pending_actions,
            };
            poll_hw_events(
                &mut state,
                &sys_conn,
                &cfg,
                mode_vdisplay as u16,
                mode_hdisplay as u16,
            );
        }

        let current_minute = get_current_minute();
        if layers[active_layer].displays_time && current_minute != last_redraw_minute {
            needs_complete_redraw = true;
            last_redraw_minute = current_minute;
        }
        if layers[active_layer].displays_battery && current_minute != last_battery_update_minute {
            for w in &mut layers[active_layer].widgets {
                if w.1.is_battery() {
                    w.1.changed = true;
                }
            }
            last_battery_update_minute = current_minute;
        }

        if should_cleanup_cache() {
            icon::cleanup_cache();
        }

        let window_cache_updated = dynamicshortcuts::check_and_reset_cache_updated();
        if dynamicshortcuts::check_and_reset_inactivity() {
            let _ = sys_conn.call_method(
                Some("org.ktouchbar.Hardware"),
                "/org/ktouchbar/Hardware",
                Some("org.ktouchbar.Hardware"),
                "ResetBacklightInactivity",
                &(),
            );
        }
        if window_cache_updated {
            let rebuild = rebuild_cached_layers(&cfg);
            cached_main = rebuild.0;
            cached_fn = rebuild.1;
            if navigation_state.current_panel.is_none() {
                layers[0] = cached_main.clone();
            }
            layers[1] = cached_fn.clone();
            needs_complete_redraw = true;
            clear_all_touches(&mut layers, &mut touches);
            release_all_held_keys(&sys_conn, &mut held_keys);
        }

        if !pending_actions.is_empty() {
            let mut state = EventState {
                layers: &mut layers,
                navigation_state: &mut navigation_state,
                touches: &mut touches,
                held_keys: &mut held_keys,
                needs_complete_redraw: &mut needs_complete_redraw,
                active_layer: &mut active_layer,
                pending_actions: &mut pending_actions,
            };
            execute_pending_actions(
                &mut state,
                &sys_conn,
                &cfg,
            );
        }

        let any_changed = layers[active_layer].widgets.iter().any(|(_, w)| {
            if w.changed {
                return true;
            }
            if let ktouchbar::widget::WidgetKind::Container(ref container) = w.kind {
                return container.children.iter().any(|c| c.changed);
            }
            false
        });
        if needs_complete_redraw || any_changed {
            let shift = if cfg.enable_pixel_shift {
                pixel_shift.get()
            } else {
                (0.0, 0.0)
            };
            let _ = layers[active_layer].draw(
                &cfg,
                mode_vdisplay as i32,
                mode_hdisplay as i32,
                &surface,
                shift,
                needs_complete_redraw,
            );
            needs_complete_redraw = false;
            send_frame_to_hw(&sys_conn, &mut surface);
        }

        if cfg.enable_pixel_shift {
            let (needs_redraw, _) = pixel_shift.update();
            if needs_redraw {
                needs_complete_redraw = true;
            }
        }

        if SIGTERM_RECEIVED.load(Ordering::SeqCst) {
            let _ = sys_conn.call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "ReleaseName",
                &(NAME_LIFETIME,),
            );
            let _ = sys_conn.call_method(
                Some("org.ktouchbar.Hardware"),
                "/org/ktouchbar/Hardware",
                Some("org.ktouchbar.Hardware"),
                "NotifyUserDisconnect",
                &(),
            );
            break;
        }

        thread::sleep(Duration::from_millis(DE_POLL_MS as u64));
    }
}

fn send_backlight_config(conn: &zbus::blocking::Connection, cfg: &Config) {
    let _ = conn.call_method(
        Some("org.ktouchbar.Hardware"),
        "/org/ktouchbar/Hardware",
        Some("org.ktouchbar.Hardware"),
        "SetBacklightConfig",
        &(
            cfg.active_brightness,
            cfg.dimmed_brightness,
            cfg.brightness_dim_timeout_ms,
            cfg.brightness_off_timeout_ms,
            cfg.adaptive_brightness,
        ),
    );
}

fn inject_key(conn: &zbus::blocking::Connection, key: input_linux::Key, pressed: bool) {
    let _ = conn.call_method(
        Some("org.ktouchbar.Hardware"),
        "/org/ktouchbar/Hardware",
        Some("org.ktouchbar.Hardware"),
        "InjectKey",
        &(key as u32, pressed),
    );
}

fn release_all_held_keys(conn: &zbus::blocking::Connection, held_keys: &mut HashSet<u32>) {
    for key_code in held_keys.drain() {
        let _ = conn.call_method(
            Some("org.ktouchbar.Hardware"),
            "/org/ktouchbar/Hardware",
            Some("org.ktouchbar.Hardware"),
            "InjectKey",
            &(key_code, false),
        );
    }
}

fn poll_hw_events(
    state: &mut EventState,
    conn: &zbus::blocking::Connection,
    cfg: &Config,
    width: u16,
    height: u16,
) {
    let fn_pressed: bool = conn
        .call_method(
            Some("org.ktouchbar.Hardware"),
            "/org/ktouchbar/Hardware",
            Some("org.ktouchbar.Hardware"),
            "GetFnState",
            &(),
        )
        .and_then(|r| r.body())
        .unwrap_or(false);

    let desired_layer = if fn_pressed { 1 } else { 0 };
    if *state.active_layer != desired_layer {
        *state.active_layer = desired_layer;
        *state.needs_complete_redraw = true;
        clear_all_touches(state.layers, state.touches);
    }

    let events: Vec<TouchEventData> = conn
        .call_method(
            Some("org.ktouchbar.Hardware"),
            "/org/ktouchbar/Hardware",
            Some("org.ktouchbar.Hardware"),
            "PollEvents",
            &(),
        )
        .and_then(|r| r.body())
        .unwrap_or_default();

    for te in &events {
        process_touch_event(state, te, conn, cfg, width, height);
    }
}

fn send_frame_to_hw(
    conn: &zbus::blocking::Connection,
    surface: &mut ImageSurface,
) {
    let data = surface.data().unwrap();
    let size = data.len();

    let memfd = match create_memfd("ktouchbar-frame", size) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("ktouchbar-user: failed to create memfd: {e}");
            return;
        }
    };

    let mut file = File::from(memfd);
    let _ = file.write_all(&data);
    let _ = file.seek(SeekFrom::Start(0));

    let fd = file.as_raw_fd();
    let zvfd = zbus::zvariant::Fd::from(fd);
    let dup_fd = unsafe { libc::dup(fd) };
    if dup_fd >= 0 {
        let _ = conn.call_method(
            Some("org.ktouchbar.Hardware"),
            "/org/ktouchbar/Hardware",
            Some("org.ktouchbar.Hardware"),
            "PushFrame",
            &(zvfd,),
        );
        unsafe { libc::close(dup_fd) };
    }
}

fn create_memfd(name: &str, size: usize) -> std::io::Result<OwnedFd> {
    let cname = std::ffi::CString::new(name).unwrap();
    let fd = unsafe { libc::memfd_create(cname.as_ptr(), 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    unsafe {
        libc::ftruncate(fd, size as i64);
    }
    Ok(unsafe { <OwnedFd as FromRawFd>::from_raw_fd(fd) })
}

fn process_touch_event(
    state: &mut EventState,
    te: &TouchEventData,
    conn: &zbus::blocking::Connection,
    cfg: &Config,
    width: u16,
    height: u16,
) {
    let active_layer = *state.active_layer;
    match te.event_type {
        0 => {
            if let Some((w_idx, c_idx)) =
                state.layers[active_layer].hit(width, height, te.x, te.y, None)
            {
                state.touches.insert(te.slot, (active_layer, w_idx, c_idx));
                let is_slider = state.layers[active_layer].get_widget(w_idx, c_idx).is_slider();

                if is_slider {
                    let pos = state.layers[active_layer].widget_position(width, w_idx);
                    let child_pos = state.layers[active_layer].child_position(width, w_idx, c_idx);
                    let w = state.layers[active_layer].get_widget_mut(w_idx, c_idx);
                    w.set_dragging(true);
                    if let Some((wx, ww)) = child_pos.or(pos) {
                        w.update_slider_value(te.x, wx, ww);
                    }
                    w.active = true;
                } else {
                    let old_active = {
                        let widget = state.layers[active_layer].get_widget(w_idx, c_idx);
                        widget.active
                    };
                    if !old_active {
                        let action = state.layers[active_layer].get_widget(w_idx, c_idx).action.clone();
                        let w = state.layers[active_layer].get_widget_mut(w_idx, c_idx);
                        w.active = true;
                        w.changed = true;
                        if c_idx.is_some() {
                            state.layers[active_layer].widgets[w_idx].1.changed = true;
                        }
                        handle_widget_action(
                            state, conn, cfg, &action, true,
                            Some(w_idx), c_idx,
                        );
                    }
                }
                state.navigation_state.update_interaction_time();
            }
        }
        1 => {
            if let Some(&(layer, w_idx, c_idx)) = state.touches.get(&te.slot) {
                let is_slider = state.layers[layer].get_widget(w_idx, c_idx).dragging();

                if is_slider {
                    let pos = state.layers[active_layer].child_position(width, w_idx, c_idx)
                        .or_else(|| state.layers[active_layer].widget_position(width, w_idx));
                    let w = state.layers[layer].get_widget_mut(w_idx, c_idx);
                    if let Some((wx, ww)) = pos {
                        w.update_slider_value(te.x, wx, ww);
                        if w.live_update() {
                            w.write_slider_value();
                        }
                    }
                    state.navigation_state.update_interaction_time();
                } else {
                    let hit = state.layers[active_layer]
                        .hit(width, height, te.x, te.y, Some(w_idx))
                        .and_then(|(_, c)| c)
                        .is_none_or(|c| c == c_idx.unwrap_or(0));
                    let old_active = {
                        let w = state.layers[layer].get_widget(w_idx, c_idx);
                        w.active
                    };
                    if old_active != hit {
                        let action = state.layers[layer].get_widget(w_idx, c_idx).action.clone();
                        let w = state.layers[layer].get_widget_mut(w_idx, c_idx);
                        w.active = hit;
                        w.changed = true;
                        if c_idx.is_some() {
                            state.layers[layer].widgets[w_idx].1.changed = true;
                        }
                        handle_widget_action(
                            state, conn, cfg, &action, hit,
                            Some(w_idx), c_idx,
                        );
                    }
                    state.navigation_state.update_interaction_time();
                }
            }
        }
        2 => {
            if let Some((layer, w_idx, c_idx)) = state.touches.remove(&te.slot) {
                let was_dragging = state.layers[layer].get_widget(w_idx, c_idx).dragging();

                if was_dragging {
                    let w = state.layers[layer].get_widget_mut(w_idx, c_idx);
                    w.commit_slider_value();
                    w.active = false;
                } else {
                    let old_active = {
                        let w = state.layers[layer].get_widget(w_idx, c_idx);
                        w.active
                    };
                    if old_active {
                        let action = state.layers[layer].get_widget(w_idx, c_idx).action.clone();
                        let w = state.layers[layer].get_widget_mut(w_idx, c_idx);
                        w.active = false;
                        w.changed = true;
                        if c_idx.is_some() {
                            state.layers[layer].widgets[w_idx].1.changed = true;
                        }
                        handle_widget_action(
                            state, conn, cfg, &action, false,
                            Some(w_idx), c_idx,
                        );
                    }
                }
                state.navigation_state.update_interaction_time();
            }
        }
        _ => {}
    }
}

#[derive(Clone, Debug)]
struct NavigationState {
    navigation_stack: Vec<String>,
    current_panel: Option<String>,
    last_interaction_time: Instant,
}

impl NavigationState {
    fn new() -> Self {
        NavigationState {
            navigation_stack: Vec::new(),
            current_panel: None,
            last_interaction_time: Instant::now(),
        }
    }
    fn push_panel(&mut self, panel_name: String) {
        if let Some(current) = self.current_panel.replace(panel_name) {
            self.navigation_stack.push(current);
        }
        self.last_interaction_time = Instant::now();
    }
    fn pop_panel(&mut self) -> bool {
        if let Some(previous) = self.navigation_stack.pop() {
            self.current_panel = Some(previous);
            self.last_interaction_time = Instant::now();
            true
        } else if self.current_panel.is_some() {
            self.current_panel = None;
            self.last_interaction_time = Instant::now();
            true
        } else {
            false
        }
    }
    fn reset_to_main(&mut self) {
        self.navigation_stack.clear();
        self.current_panel = None;
        self.last_interaction_time = Instant::now();
    }
    fn update_interaction_time(&mut self) {
        self.last_interaction_time = Instant::now();
    }
    fn should_timeout(&self, timeout_seconds: u32) -> bool {
        timeout_seconds > 0
            && self.current_panel.is_some()
            && self.last_interaction_time.elapsed().as_secs() >= timeout_seconds as u64
    }
}

#[derive(Clone, Debug)]
struct PendingAction {
    action: Option<Action>,
    execution_time: Instant,
    widget_index: usize,
    child_index: Option<usize>,
    layer_index: usize,
}

struct EventState<'a> {
    layers: &'a mut [FunctionLayer; 2],
    navigation_state: &'a mut NavigationState,
    touches: &'a mut HashMap<u32, (usize, usize, Option<usize>)>,
    held_keys: &'a mut HashSet<u32>,
    needs_complete_redraw: &'a mut bool,
    active_layer: &'a mut usize,
    pending_actions: &'a mut Vec<PendingAction>,
}

fn handle_widget_action(
    state: &mut EventState,
    conn: &zbus::blocking::Connection,
    config: &Config,
    action: &Option<Action>,
    active: bool,
    widget_index: Option<usize>,
    child_index: Option<usize>,
) {
    let action = match action {
        Some(a) => a,
        None => return,
    };

    match action {
        Action::Key(keys) => {
            if active {
                release_all_held_keys(conn, state.held_keys);
                for key in keys {
                    inject_key(conn, *key, true);
                    state.held_keys.insert(*key as u32);
                }
            } else {
                for key in keys.iter().rev() {
                    inject_key(conn, *key, false);
                    state.held_keys.remove(&(*key as u32));
                }
            }
        }
        Action::Back => {
            if active {
                if let Some(widx) = widget_index {
                    state.pending_actions.push(PendingAction {
                        action: Some(Action::Back),
                        execution_time: Instant::now() + Duration::from_millis(150),
                        widget_index: widx,
                        child_index,
                        layer_index: 0,
                    });
                } else {
                    if state.navigation_state.pop_panel() {
                        update_layer_for_navigation(
                            conn, state.navigation_state, config, state.layers,
                            state.needs_complete_redraw, state.touches, state.held_keys,
                        );
                    }
                }
            }
        }
        Action::Panel(panel_name) => {
            if active {
                if let Some(widx) = widget_index {
                    state.pending_actions.push(PendingAction {
                        action: Some(Action::Panel(panel_name.clone())),
                        execution_time: Instant::now() + Duration::from_millis(150),
                        widget_index: widx,
                        child_index,
                        layer_index: 0,
                    });
                } else {
                    state.navigation_state.push_panel(panel_name.clone());
                    update_layer_for_navigation(
                        conn, state.navigation_state, config, state.layers,
                        state.needs_complete_redraw, state.touches, state.held_keys,
                    );
                }
            }
        }
        Action::Exec(command) => {
            if active {
                if let Some(widx) = widget_index {
                    state.pending_actions.push(PendingAction {
                        action: Some(Action::Exec(command.clone())),
                        execution_time: Instant::now() + Duration::from_millis(150),
                        widget_index: widx,
                        child_index,
                        layer_index: 0,
                    });
                } else {
                    let _ = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(command)
                        .spawn();
                }
            }
        }
    }
}

fn execute_pending_actions(
    state: &mut EventState,
    conn: &zbus::blocking::Connection,
    config: &Config,
) {
    let now = Instant::now();
    let mut expired: Vec<PendingAction> = Vec::new();
    let mut remaining: Vec<PendingAction> = Vec::new();
    for pa in state.pending_actions.drain(..) {
        if now >= pa.execution_time {
            expired.push(pa);
        } else {
            remaining.push(pa);
        }
    }
    *state.pending_actions = remaining;
    for pa in &expired {
        if pa.layer_index < state.layers.len()
            && pa.widget_index < state.layers[pa.layer_index].widgets.len()
        {
            let w = state.layers[pa.layer_index].get_widget_mut(pa.widget_index, pa.child_index);
            w.active = false;
            w.changed = true;
            if pa.child_index.is_some() {
                state.layers[pa.layer_index].widgets[pa.widget_index].1.changed = true;
            }
        }
        match &pa.action {
            Some(Action::Back)
                if state.navigation_state.pop_panel() => {
                    update_layer_for_navigation(
                        conn, state.navigation_state, config, state.layers,
                        state.needs_complete_redraw, state.touches, state.held_keys,
                    );
                }
            Some(Action::Panel(name)) => {
                state.navigation_state.push_panel(name.clone());
                update_layer_for_navigation(
                    conn, state.navigation_state, config, state.layers,
                    state.needs_complete_redraw, state.touches, state.held_keys,
                );
            }
            Some(Action::Exec(cmd)) => {
                let _ = std::process::Command::new("sh").arg("-c").arg(cmd).spawn();
            }
            _ => {}
        }
    }
}

fn update_layer_for_navigation(
    conn: &zbus::blocking::Connection,
    navigation_state: &NavigationState,
    config: &Config,
    layers: &mut [FunctionLayer; 2],
    needs_complete_redraw: &mut bool,
    touches: &mut HashMap<u32, (usize, usize, Option<usize>)>,
    held_keys: &mut HashSet<u32>,
) {
    if let Some(panel_name) = &navigation_state.current_panel {
        if let Some(panel_widgets) = config.panels.get(panel_name) {
            layers[0] = build_layer(panel_widgets, config);
            *needs_complete_redraw = true;
            clear_all_touches(layers, touches);
            release_all_held_keys(conn, held_keys);
        }
    } else {
        let rebuild = rebuild_cached_layers(config);
        layers[0] = rebuild.0;
        layers[1] = rebuild.1;
        *needs_complete_redraw = true;
        clear_all_touches(layers, touches);
        release_all_held_keys(conn, held_keys);
    }
}

fn clear_all_touches(
    layers: &mut [FunctionLayer; 2],
    touches: &mut HashMap<u32, (usize, usize, Option<usize>)>,
) {
    if touches.is_empty() {
        return;
    }
    for layer in layers.iter_mut() {
        for (_, w) in layer.widgets.iter_mut() {
            if let ktouchbar::widget::WidgetKind::Container(ref mut state) = w.kind {
                for child in &mut state.children {
                    if child.active {
                        child.active = false;
                        child.changed = true;
                    }
                    if child.dragging() {
                        child.set_dragging(false);
                        child.commit_slider_value();
                    }
                }
            }
            if w.active {
                w.active = false;
                w.changed = true;
            }
            if w.dragging() {
                w.set_dragging(false);
                w.commit_slider_value();
            }
        }
    }
    touches.clear();
}
