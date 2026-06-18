// SPDX-License-Identifier: GPL-3.0-only
use chrono::{Local, Timelike};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SystemState {
    pub current_minute: u32,
    pub cache_cleanup_due: bool,
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

pub struct SystemMonitor {
    _handle: thread::JoinHandle<()>,
}

impl Default for SystemMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemMonitor {
    pub fn new() -> Self {
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

// Public API
pub fn get_current_minute() -> u32 {
    if let Ok(state) = SYSTEM_STATE.lock() {
        state.current_minute
    } else {
        Local::now().minute()
    }
}

pub fn should_cleanup_cache() -> bool {
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

