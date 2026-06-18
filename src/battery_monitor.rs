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

        let capacity = Self::read_capacity(battery);

        let state = match status.trim() {
            "Charging" | "Full" => BatteryState::Charging,
            "Discharging" if capacity < 10 => BatteryState::Low,
            _ => BatteryState::NotCharging,
        };

        BatteryInfo { capacity, state }
    }

    fn read_capacity(battery: &str) -> u32 {
        // Try the universal capacity file first (always 0-100)
        let capacity_path = format!("/sys/class/power_supply/{}/capacity", battery);
        if let Ok(cap) = fs::read_to_string(&capacity_path)
            .and_then(|s| s.trim().parse::<u32>().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)))
        {
            return cap;
        }

        // Fallback: compute from charge_now/charge_full
        let charge_now_path = format!("/sys/class/power_supply/{}/charge_now", battery);
        let charge_full_path = format!("/sys/class/power_supply/{}/charge_full", battery);

        let charge_now = fs::read_to_string(&charge_now_path)
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok());
        let charge_full = fs::read_to_string(&charge_full_path)
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok());

        if let (Some(now), Some(full)) = (charge_now, charge_full) {
            if full > 0.0 {
                return ((now / full) * 100.0).round() as u32;
            }
        }

        // Fallback: compute from energy_now/energy_full
        let energy_now_path = format!("/sys/class/power_supply/{}/energy_now", battery);
        let energy_full_path = format!("/sys/class/power_supply/{}/energy_full", battery);

        let energy_now = fs::read_to_string(&energy_now_path)
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok());
        let energy_full = fs::read_to_string(&energy_full_path)
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok());

        if let (Some(now), Some(full)) = (energy_now, energy_full) {
            if full > 0.0 {
                return ((now / full) * 100.0).round() as u32;
            }
        }

        100
    }
}

// Public API
pub fn get_cached_battery_state() -> Option<(u32, BatteryState)> {
    if let Ok(state) = BATTERY_STATE.lock() {
        state.as_ref().map(|info| (info.capacity, info.state))
    } else {
        None
    }
}

