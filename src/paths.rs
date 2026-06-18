// SPDX-License-Identifier: GPL-3.0-only
use std::path::PathBuf;

pub fn user_config_dir() -> PathBuf {
    if std::env::var_os("HOME").is_some() {
        freedesktop::xdg_config_home().join("ktouchbar")
    } else {
        PathBuf::from("/etc/ktouchbar")
    }
}

pub fn slider_icons_dir() -> PathBuf {
    user_config_dir().join("icons")
}
