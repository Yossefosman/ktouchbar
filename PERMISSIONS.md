# KTouchBar Permissions Map

## Overview

Two-process split architecture:

```
┌───────────────────────────────────────────────────────────────────────┐
│  ktouchbar-system   (root, systemd system service)                    │
│  /usr/bin/ktouchbar-system                                            │
│  Runs as root via: systemd -> ktouchbar-system.service                │
├───────────────────────────────────────────────────────────────────────┤
│  Opens /dev/dri/card*  (DRM master)                                   │
│  Opens /dev/uinput × 2 (virtual keyboards)                            │
│  Opens /dev/input/* via libinput (touch input, Fn key)                │
│  Writes /sys/class/backlight/*/brightness                             │
└──────────────────────┬────────────────────────────────────────────────┘
                       │ D-Bus system bus (org.ktouchbar.Hardware)
                       │ PushFrame(fd), PollEvents(), GetFnState(),
                       │ InjectKey(), SetBacklightConfig()
                       ▼
┌───────────────────────────────────────────────────────────────────────┐
│  ktouchbar-user      (user, systemd user service)                     │
│  /usr/bin/ktouchbar-user                                              │
│  Runs as desktop user via: systemd --user -> ktouchbar-user.service   │
├───────────────────────────────────────────────────────────────────────┤
│  Renders with Cairo, sends frames via memfd + D-Bus                   │
│  Reads ~/.config/ktouchbar/configs/config.toml                        │
│  Reads /usr/share/ktouchbar/configs/config.toml                       │
│  Reads /sys/class/power_supply/* (battery, world-readable)            │
│  Runs pactl (volume control)                                          │
│  Reads/writes via KDE D-Bus (brightness)                              │
│  Connects to session D-Bus (KWin window tracking)                     │
└───────────────────────────────────────────────────────────────────────┘
```

---

## 1. DRM Display Control (ktouchbar-system)

**Purpose:** Opens and masters a DRM device for the Touch Bar display (atomic modesetting, buffer swaps).

**Access:** Root via systemd system service — no additional capabilities needed.

```
  display.rs (ktouchbar library)
       │
       ▼
  /dev/dri/card*
       │
       ├── DRM master lock (drmSetMaster)
       ├── Atomic modeset (planes, CRTC, connectors)
       └── Dumb buffer + framebuffer creation
```

**Required by:** `ktouchbar-system` (system daemon)

**Related code:** `src/display.rs` — `DrmBackend::open_card()`, `try_open_card()`

**Related udev rules:**
- `99-touchbar-seat.rules` — Tags DRM devices with `DRIVERS=="adp|appletbdrm"` to seat-touchbar (isolates from desktop compositor)
- `99-touchbar-ktouchbar.rules` — Creates systemd alias `/dev/ktouchbar_display`

**Notes:**
- The old `setcap cap_sys_admin+ep` approach is **not used** — `ktouchbar-system` has natural root access via systemd
- The two udev rules above are **optional** — the system daemon can find the right DRM card without them (iterates all cards until it finds one connected)

---

## 2. Virtual Input Device (/dev/uinput) (ktouchbar-system)

**Purpose:** Creates two virtual keyboards:
1. "KTouchBar Hardware Virtual Input Device" — F1-F12 only (fallback mode)
2. "KTouchBar Virtual Keyboard" — all KEY_CNT keys (user key injection)

**Access:** Root via systemd — no udev mode changes needed.

```
  system.rs
       │
       ├── /dev/uinput ──► "KTouchBar Hardware Virtual Input Device" (F1-F12)
       │                         Used when user service is disconnected
       │
       └── /dev/uinput ──► "KTouchBar Virtual Keyboard" (all keys)
                                Used for InjectKey D-Bus calls from user
```

**Required by:** `ktouchbar-system`

**Related code:** `src/bin/system.rs:206-279`

**Related udev rules:**
- `74-ktouchbar-uinput.rules` (`MODE="0666", TAG+="uaccess"`) — **NOT needed** when system daemon runs as root

---

## 3. Touch Input Device (libinput) (ktouchbar-system)

**Purpose:** Reads touch/gesture events from the Touch Bar digitizer and keyboard events (Fn key detection) from the main input seat.

**Access:** Root via systemd.

```
  system.rs (libinput dispatch)
       │
       ├── libinput (seat-touchbar)  ──► /dev/input/event* (Touch Bar digitizer)
       │
       └── libinput (seat0)          ──► /dev/input/event* (Fn key, lid switch)
```

**Touch event flow:**
```
  libinput ──► system.rs queues TouchEventRecord ──► D-Bus PollEvents()
       │                                                  │
       │                                           ktouchbar-user
       │                                           calls PollEvents
       ▼                                                  ▼
  Or (fallback mode): system daemon handles directly
```

**Related code:** `src/bin/system.rs:317-320`, `src/bin/system.rs:461-499`

**Related udev rules:**
- `99-touchbar-seat.rules` — **REQUIRED**: assigns `ID_SEAT="seat-touchbar"` to the Touch Bar input device (by device name), so libinput's `seat-touchbar` context finds it
- `40-ktouchbar-permissions.rules` line 7 (`MODE="0666"` on `Touch Bar Display` input) — **NOT needed** (system daemon runs as root and can open input devices)

---

## 4. Backlight Control (ktouchbar-system)

**Purpose:** Controls Touch Bar brightness with configurable dim/off timeouts and optional adaptive brightness (follows main display).

**Access:** Root via systemd.

```
  backlight.rs (ktouchbar library, used by system daemon)
       │
       ▼
  /sys/class/backlight/
       ├── appletb_backlight/            (T2 Macs)
       │     ├── brightness        (write)
       │     └── max_brightness    (read)
       ├── 228200000.display-pipe.0/     (Apple Silicon M1/M2)
       │     ├── brightness        (write)
       │     └── max_brightness    (read)
       └── 228600000.dsi.0/             (Apple Silicon alternate DSI)
             ├── brightness        (write)
             └── max_brightness    (read)
```

**Adaptive brightness** also reads the main display backlight:
```
  /sys/class/backlight/<display>/
       └── brightness (read, world-readable by default)
```

**During startup** the initial brightness is set to max via `std::fs::write()` (line 93), then managed via the open `File` handle for the rest of the session.

**Related code:** `src/backlight.rs` — all

**Related udev rules:**
- `40-ktouchbar-permissions.rules` lines 2-4 (`chmod 0666` on backlight brightness) — **NOT needed** (system daemon runs as root)
- **However**, the initial startup write on line 93 uses `std::fs::write()` (not the open handle), which may fail if the backlight file's permissions are restrictive. It works as root.

---

## 5. Frame Transfer (ktouchbar-user → ktouchbar-system)

**Purpose:** The user service renders widgets with Cairo and sends the raw RGBA frame to the system daemon for display on the Touch Bar.

```
  ktouchbar-user:
       │
       ├── Cairo ImageSurface (RGBA)
       ├── memfd_create() ──► anonymous fd
       ├── write surface bytes to memfd
       └── D-Bus PushFrame(fd) ──► system copies frame_pixels
                                       │
                                       ▼
                              ktouchbar-system blits to DRM
```

**No special permissions needed** — memfd is in-memory, fd passing via D-Bus is standard.

**Related code:** `src/bin/user.rs:462-506`, `src/bin/system.rs:137-159`

---

## 6. Config File Access (ktouchbar-user)

**Purpose:** Reads TOML configuration for widget layout, global options, panels, and app rules.

```
  config.rs
       │
       ├── /usr/share/ktouchbar/configs/config.toml       (system defaults)
       │     (world-readable, installed by package)
       │
       └── ~/.config/ktouchbar/configs/config.toml        (user overrides)
             (user's own file, hot-reloaded via inotify)
```

**Access:** Natural user access (both paths are readable by the desktop user).

**Related code:** `src/config.rs` — `load_config()`, `ConfigManager`

---

## 7. Power Supply / Battery Monitoring (ktouchbar-user)

**Purpose:** Reads battery percentage and charging status for the battery widget.

```
  battery_monitor.rs (background thread, every 30s)
       │
       ▼
  /sys/class/power_supply/<device>/
       ├── status          (read, world-readable)
       ├── charge_now      (read, world-readable)
       ├── charge_full     (read, world-readable)
       ├── capacity        (read, world-readable)
       └── type            (read, world-readable)
```

**No special permissions needed** — all world-readable by default.

**Related code:** `src/battery_monitor.rs`, `src/widget.rs:445-468`

---

## 8. Volume Control (ktouchbar-user)

**Purpose:** Reads/writes system audio volume via PulseAudio/PipeWire pactl.

```
  slider.rs (user daemon)
       │
       ├── Read:  pactl get-sink-volume @DEFAULT_SINK@
       └── Write: pactl set-sink-volume @DEFAULT_SINK@ <pct>%
```

**Access:** Natural user access — PulseAudio/PipeWire permissions are per-user.

**Related code:** `src/slider.rs` — `read_slider_value()`, `write_slider_value()`

---

## 9. Display Brightness Control (ktouchbar-user)

**Purpose:** Reads/writes main display brightness via KDE Power Management D-Bus API.

```
  slider.rs (user daemon)
       │
       ├── Read:  org.kde.Solid.PowerManagement.Actions.BrightnessControl.brightness
       └── Write: org.kde.Solid.PowerManagement.Actions.BrightnessControl.setBrightness()
```

**Access:** D-Bus — accessible to the desktop user session.

**Related code:** `src/slider.rs`

---

## 10. Keyboard Brightness Control (ktouchbar-user)

**Purpose:** Reads/writes keyboard backlight brightness via KDE D-Bus API.

```
  slider.rs (user daemon)
       │
       ├── Read:  org.kde.Solid.PowerManagement.Actions.KeyboardBrightnessControl.keyboardBrightness
       └── Write: org.kde.Solid.PowerManagement.Actions.KeyboardBrightnessControl.setKeyboardBrightness()
```

**Access:** D-Bus — accessible to the desktop user session.

**Related code:** `src/slider.rs`

---

## 11. Icon Loading (ktouchbar-user)

**Purpose:** Loads SVG/PNG icons from freedesktop icon themes for widget display.

```
  icon_cache.rs (background preloader + sync loader)
       │
       ├── ~/.local/share/icons/<theme>/          (user local icons)
       ├── $XDG_DATA_DIRS/icons/<theme>/          (system icons)
       ├── ~/.config/ktouchbar/icons/             (custom user icons)
       └── Absolute paths from config             (arbitrary paths)
```

**Access:** Natural user access — all standard icon paths are world-readable or in user home.

**Related code:** `src/icon_cache.rs`

---

## 12. User Session / D-Bus Detection (ktouchbar-user)

**Purpose:** Detects the desktop user's session D-Bus address for KWin integration.

```
  user_cache.rs
       │
       ├── loginctl list-sessions          (subprocess)
       ├── loginctl show-session <id>      (subprocess)
       ├── /run/user/<uid>/                (directory listing)
       ├── /run/user/<uid>/wayland-0       (socket check)
       ├── /run/user/<uid>/bus              (D-Bus socket)
       ├── getent passwd <uid>             (subprocess)
       └── id -u <username>                (subprocess)
```

**Access:** Natural user access — runs as desktop user, all within the user's session.

**Related code:** `src/user_cache.rs`

---

## 13. KWin Script Integration (ktouchbar-user)

**Purpose:** Receives active window information from KWin for dynamic shortcut switching.

```
  KWin script (JavaScript)
       │
       ├── windowActivated / clientActivated
       │
       ▼
  D-Bus session bus: org.ktouchbar.DynamicShortcasts.SetActiveWindow(class, title)
       │
       ▼
  dynamicshortcuts.rs (user daemon)
```

**Access:** Session D-Bus — naturally accessible to the user's own processes. The KWin script runs in the user's KWin instance.

**Related code:** `src/dynamicshortcuts.rs`, `share/kwin/scripts/ktouchbar_dynamicshortcuts/contents/code/main.js`

---

## 14. Action Execution (ktouchbar-user)

**Purpose:** Executes shell commands when a widget with `Action::Exec(command)` is tapped.

```
  user.rs line ~755-768:
       │
       └── std::process::Command::new("sh")
               .args(["-c", &command])
               .envs(imported_env)    // DISPLAY, WAYLAND_DISPLAY from systemd
               .spawn()
```

**Access:** Runs as the desktop user — all actions execute with the user's privileges.

**Related code:** `src/bin/user.rs:698-768`

---

## Permission Flow Summary

```
  ┌───────────────────────────────────────────────────────────┐
  │  ktouchbar-system  (systemd -- system)                    │
  │  Runs as root                                             │
  │                                                           │
  │  ● /dev/dri/card*     — DRM master (natural root access) │
  │  ● /dev/uinput ×2    — Virtual keyboards (root)          │
  │  ● /dev/input/*      — libinput (root)                   │
  │  ● /sys/class/backlight — Brightness (root)              │
  └───────────────────────────┬───────────────────────────────┘
                              │ D-Bus (system bus)
                              │ org.ktouchbar.Hardware
                              │
  ┌───────────────────────────▼───────────────────────────────┐
  │  ktouchbar-user  (systemd -- user)                        │
  │  Runs as desktop user                                     │
  │                                                           │
  │  ● No special permissions needed                          │
  │  ● Reads/writes via D-Bus, pactl, sysfs (world-r)        │
  │  ● Reads config/icons from home + system paths            │
  │  ● Connects to session D-Bus (KWin)                       │
  │  ● Runs commands as desktop user                          │
  └───────────────────────────────────────────────────────────┘
```

## Files Involved

| File | Role | Needed? |
|------|------|---------|
| `src/display.rs` | Opens DRM device, acquires master | Required |
| `src/bin/system.rs:206-279` | Opens `/dev/uinput` × 2 | Required |
| `src/bin/system.rs:317-320` | libinput interfaces (touch + Fn) | Required |
| `src/backlight.rs` | Backlight read/write | Required |
| `src/bin/user.rs` | Frame rendering, touch handling, actions | Required |
| `src/config.rs` | Config loading/hot-reload | Required |
| `src/battery_monitor.rs` | Battery status read (world) | Required |
| `src/slider.rs` | Volume/brightness via pactl + D-Bus | Required |
| `src/user_cache.rs` | User session detection | Required |
| `src/dynamicshortcuts.rs` | D-Bus KWin integration | Required |
| `src/icon_cache.rs` | Icon theme loading | Required |
| `etc/udev/rules.d/99-touchbar-seat.rules` | Seat isolation for libinput | **Required** |
| `etc/udev/rules.d/99-touchbar-ktouchbar.rules` | USB config + systemd aliases | Optional (USB config for hardware, aliases for ordering) |
| `etc/udev/rules.d/40-ktouchbar-permissions.rules` | chmod backlight + input MODE | **Not needed** (system daemon is root) |
| `etc/udev/rules.d/74-ktouchbar-uinput.rules` | uinput MODE="0666" | **Not needed** (system daemon is root) |
| `etc/dbus-1/system.d/org.ktouchbar.Hardware.conf` | D-Bus policy | Required |
| `etc/systemd/system/ktouchbar-system.service` | System daemon unit | Required |
| `etc/systemd/user/ktouchbar-user.service` | User daemon unit | Required |
| `ktouchbar.install` | Post-install hooks | Required |
