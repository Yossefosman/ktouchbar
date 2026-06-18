// SPDX-License-Identifier: GPL-3.0-only
use anyhow::Result;
use serde::{Serialize, Deserialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use zbus::dbus_interface;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveWindowInfo {
    pub title: String,
    pub class: String,
}

static CACHED_WINDOW_INFO: std::sync::LazyLock<Mutex<Option<ActiveWindowInfo>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

static CACHE_UPDATED: std::sync::LazyLock<Mutex<bool>> =
    std::sync::LazyLock::new(|| Mutex::new(false));

pub static INACTIVITY_RESET: AtomicBool = AtomicBool::new(false);

struct DynamicShortcutsService;

#[dbus_interface(name = "org.ktouchbar.DynamicShortcuts")]
impl DynamicShortcutsService {
    fn set_active_window(&mut self, class: String, title: String) {
        let info = ActiveWindowInfo { title, class };
        if let Ok(mut guard) = CACHED_WINDOW_INFO.lock() {
            let changed = guard.as_ref().is_none_or(|c| {
                c.class != info.class || c.title != info.title
            });
            if changed {
                *guard = Some(info);
                drop(guard);
                if let Ok(mut updated) = CACHE_UPDATED.lock() {
                    *updated = true;
                }
            }
        }
    }

    fn reset_inactivity(&mut self) {
        INACTIVITY_RESET.store(true, Ordering::Relaxed);
    }
}

pub fn init() {
    std::thread::Builder::new()
        .name("dynamicshortcuts-dbus".into())
        .spawn(|| loop {
            match try_init_dbus() {
                Ok(()) => return,
                Err(e) => {
                    eprintln!("dynamicshortcuts: D-Bus init failed: {e}");
                    eprintln!("dynamicshortcuts: retrying in 5s...");
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        })
        .ok();
}

fn find_session_bus_address() -> Option<String> {
    let username = crate::user_cache::get_cached_user_environment()?.username;
    let uid_str = std::process::Command::new("id")
        .args(["-u", &username])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    let path = format!("/run/user/{}/bus", uid_str.trim());
    if std::path::Path::new(&path).exists() {
        Some(format!("unix:path={}", path))
    } else {
        None
    }
}

fn try_init_dbus() -> Result<()> {
    let conn = match zbus::blocking::Connection::session() {
        Ok(c) => c,
        Err(_) => {
            let addr = find_session_bus_address()
                .ok_or_else(|| anyhow::anyhow!("no session bus available"))?;
            zbus::blocking::ConnectionBuilder::address(addr.as_str())
                .map_err(|e| anyhow::anyhow!("failed to connect: {e}"))?
                .build()
                .map_err(|e| anyhow::anyhow!("failed to build connection: {e}"))?
        }
    };
    conn.request_name("org.ktouchbar.DynamicShortcuts")?;
    conn.object_server()
        .at("/org/ktouchbar/DynamicShortcuts", DynamicShortcutsService)?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(u64::MAX));
    }
}

pub fn get_active_window_info() -> anyhow::Result<ActiveWindowInfo> {
    if let Ok(cache) = CACHED_WINDOW_INFO.lock() {
        if let Some(ref info) = *cache {
            return Ok(info.clone());
        }
    }
    Err(anyhow::anyhow!(
        "no active window info yet (install ktouchbar_dynamicshortcuts KWin script)"
    ))
}

pub fn check_and_reset_cache_updated() -> bool {
    if let Ok(mut updated) = CACHE_UPDATED.lock() {
        let was = *updated;
        *updated = false;
        was
    } else {
        false
    }
}

pub fn check_and_reset_inactivity() -> bool {
    INACTIVITY_RESET.swap(false, Ordering::Relaxed)
}
