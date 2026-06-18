// SPDX-License-Identifier: GPL-3.0-only
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct CachedUserEnvironment {
    pub username: String,
}

static USER_ENV_CACHE: std::sync::LazyLock<Mutex<Option<CachedUserEnvironment>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

pub fn get_cached_user_environment() -> Option<CachedUserEnvironment> {
    if let Ok(cache) = USER_ENV_CACHE.lock() {
        cache.clone()
    } else {
        None
    }
}

pub fn initialize_user_environment_cache() {
    let user = detect_desktop_user().map(|username| CachedUserEnvironment { username });
    if let Ok(mut cache) = USER_ENV_CACHE.lock() {
        *cache = user;
    }
}

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
