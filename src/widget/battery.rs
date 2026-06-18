// SPDX-License-Identifier: GPL-3.0-only
use std::fs;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BatteryState {
    NotCharging,
    Charging,
    Low,
}

#[derive(Debug, Clone)]
pub struct BatteryInfo {
    pub capacity: u32,
    pub state: BatteryState,
}

static BATTERY_STATE: std::sync::LazyLock<Mutex<Option<BatteryInfo>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

pub struct BatteryMonitor {
    _handle: thread::JoinHandle<()>,
}

impl BatteryMonitor {
    pub fn new(battery_name: String) -> Self {
        let handle = thread::spawn(move || {
            Self::monitor_loop(&battery_name);
        });
        BatteryMonitor { _handle: handle }
    }

    fn monitor_loop(battery_name: &str) {
        loop {
            let battery_info = Self::read_battery_state(battery_name);
            if let Ok(mut state) = BATTERY_STATE.lock() {
                *state = Some(battery_info);
            }
            thread::sleep(Duration::from_secs(30));
        }
    }

    fn read_battery_state(battery: &str) -> BatteryInfo {
        let status_path = format!("/sys/class/power_supply/{}/status", battery);
        let status = fs::read_to_string(&status_path)
            .unwrap_or_else(|_| "Unknown".to_string());
        let capacity = read_raw_capacity(battery);
        let state = match status.trim() {
            "Charging" | "Full" => BatteryState::Charging,
            "Discharging" if capacity < 10 => BatteryState::Low,
            _ => BatteryState::NotCharging,
        };
        BatteryInfo { capacity, state }
    }
}

fn get_cached_battery_state() -> Option<(u32, BatteryState)> {
    if let Ok(state) = BATTERY_STATE.lock() {
        state.as_ref().map(|info| (info.capacity, info.state))
    } else {
        None
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
    if let Some(cached) = get_cached_battery_state() {
        return cached;
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
