// SPDX-License-Identifier: GPL-3.0-only
use anyhow::{anyhow, Result};
use input::event::{
    switch::{Switch, SwitchEvent, SwitchState},
    Event,
};
use std::{
    cmp::min,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::Instant,
};

fn read_attr(path: &Path, attr: &str) -> Option<u32> {
    match fs::read_to_string(path.join(attr)) {
        Ok(s) => s.trim().parse::<u32>().ok(),
        Err(e) => {
            eprintln!("ktouchbar: failed to read {}/{}: {}", path.display(), attr, e);
            None
        }
    }
}

fn find_backlight() -> Result<PathBuf> {
    for entry in fs::read_dir("/sys/class/backlight/")? {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if ["display-pipe", "228600000.dsi.0", "appletb_backlight"]
            .iter()
            .any(|s| name.contains(s))
        {
            return Ok(entry.path());
        }
    }
    Err(anyhow!("No Touch Bar backlight device found"))
}

fn find_display_backlight() -> Option<PathBuf> {
    let known_touchbar = ["display-pipe", "228600000.dsi.0", "appletb_backlight"];
    let entries = fs::read_dir("/sys/class/backlight/").ok()?;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !known_touchbar.iter().any(|s| name.contains(s)) {
            return Some(entry.path());
        }
    }
    None
}

fn set_backlight(mut file: &File, value: u32) {
    let _ = writeln!(file, "{value}");
}

pub struct BacklightManager {
    last_active: Instant,
    max_bl: u32,
    current_bl: u32,
    lid_state: SwitchState,
    bl_file: File,
    display_bl_path: Option<PathBuf>,
    display_max_bl: Option<u32>,
    // Config params (set via D-Bus)
    active_brightness: u32,
    dimmed_brightness: u32,
    brightness_dim_timeout_ms: u32,
    brightness_off_timeout_ms: u32,
    adaptive_brightness: bool,
}

impl Default for BacklightManager {
    fn default() -> Self {
        Self::new()
    }
}

impl BacklightManager {
    pub fn new() -> BacklightManager {
        let bl_path = find_backlight().unwrap();
        let display_bl_path = find_display_backlight();
        let bl_file = OpenOptions::new()
            .write(true)
            .open(bl_path.join("brightness"))
            .unwrap();
        let max_bl = read_attr(&bl_path, "max_brightness").unwrap_or(1);
        let (display_max_bl, display_info) = match &display_bl_path {
            Some(dp) => {
                let max = read_attr(dp, "max_brightness");
                match max {
                    Some(m) => (Some(m), format!("; Display backlight: {} (max={})", dp.display(), m)),
                    None => (None, String::new()),
                }
            }
            None => (None, String::new()),
        };
        // Set max brightness at startup
        let _ = std::fs::write(bl_path.join("brightness"), max_bl.to_string());
        println!(
            "Touch Bar backlight: {} (max={}){}",
            bl_path.display(),
            max_bl,
            display_info,
        );
        BacklightManager {
            bl_file,
            lid_state: SwitchState::Off,
            max_bl,
            current_bl: max_bl,
            last_active: Instant::now(),
            display_bl_path,
            display_max_bl,
            active_brightness: max_bl,
            dimmed_brightness: 1,
            brightness_dim_timeout_ms: u32::MAX,
            brightness_off_timeout_ms: u32::MAX,
            adaptive_brightness: false,
        }
    }
    fn display_to_touchbar(&self, display_brightness: u32, active_brightness: u32) -> u32 {
        let normalized = display_brightness as f64 / self.display_max_bl.unwrap_or(1).max(1) as f64;
        let cap = active_brightness.min(self.max_bl);
        let adjusted = (normalized.powf(0.5) * cap as f64) as u32 + 1;
        adjusted.min(self.max_bl)
    }
    pub fn set_config(&mut self, active: u32, dimmed: u32, dim_timeout_ms: u32, off_timeout_ms: u32, adaptive: bool) {
        self.active_brightness = active.min(self.max_bl);
        self.dimmed_brightness = dimmed.min(self.max_bl);
        self.brightness_dim_timeout_ms = dim_timeout_ms;
        self.brightness_off_timeout_ms = off_timeout_ms;
        self.adaptive_brightness = adaptive;
    }
    pub fn process_event(&mut self, event: &Event) {
        match event {
            Event::Keyboard(_) | Event::Pointer(_) | Event::Gesture(_) | Event::Touch(_) => {
                self.last_active = Instant::now();
            }
            Event::Switch(SwitchEvent::Toggle(toggle)) => {
                if let Some(Switch::Lid) = toggle.switch() {
                    self.lid_state = toggle.switch_state();
                    println!("Lid Switch event: {:?}", self.lid_state);
                    if toggle.switch_state() == SwitchState::Off {
                        self.last_active = Instant::now();
                    }
                }
            }
            _ => {}
        }
    }
    pub fn update_backlight(&mut self) {
        let since_last_active = (Instant::now() - self.last_active).as_millis() as u64;
        let new_bl = min(
            self.max_bl,
            if self.lid_state == SwitchState::On {
                0
            } else if since_last_active < self.brightness_dim_timeout_ms as u64 {
                if self.adaptive_brightness {
                    match &self.display_bl_path {
                        Some(dp) => match read_attr(dp, "brightness") {
                            Some(display_bl) => {
                                self.display_to_touchbar(display_bl, self.active_brightness)
                            }
                            None => {
                                eprintln!(
                                    "ktouchbar: adaptive_brightness: could not read display brightness (permission denied); \
                                     falling back to active_brightness={}",
                                    self.active_brightness
                                );
                                self.active_brightness.min(self.max_bl)
                            }
                        },
                        None => {
                            eprintln!(
                                "ktouchbar: adaptive_brightness: no display backlight device found; \
                                 falling back to active_brightness={}",
                                self.active_brightness
                            );
                            self.active_brightness.min(self.max_bl)
                        }
                    }
                } else {
                    self.active_brightness.min(self.max_bl)
                }
            } else if since_last_active < self.brightness_off_timeout_ms as u64 {
                self.dimmed_brightness.min(self.max_bl)
            } else {
                0
            },
        );
        if self.current_bl != new_bl {
            self.current_bl = new_bl;
            set_backlight(&self.bl_file, self.current_bl);
        }
    }
    pub fn current_bl(&self) -> u32 {
        self.current_bl
    }
    pub fn reset_activity(&mut self) {
        self.last_active = Instant::now();
    }
}
