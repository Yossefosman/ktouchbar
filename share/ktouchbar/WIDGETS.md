# KTouchBar Widget Reference

## Widget types

Every widget is an inline TOML table tagged by `widget = "..."`:

| Widget | Tag | Description |
|--------|-----|-------------|
| Button | `widget = "button"` | Icon or text, with an action |
| Slider | `widget = "slider"` | Draggable slider (volume/brightness) |
| Battery | `widget = "battery"` | Battery level display |
| Clock | `widget = "clock"` | Time/date display |
| Dynamic | `widget = "dynamic"` | App-specific shortcuts via KWin |
| Container | `widget = "container"` | Groups widgets with shared outline |

### Common fields

These fields are available on **every** widget type:

| Field | Type | Description |
|-------|------|-------------|
| `show_outline` | bool | Override global outline for this widget |
| `outline_color` | float or [float] | Outline color override |
| `active_color` | float or [float] | Highlight color when pressed |
| `padding` | int or "default" | Padding in pixels (fixed width) |
| `padding_left` | int or "default" | Left padding only |
| `padding_right` | int or "default" | Right padding only |
| `margin` | int | External spacing on both sides |
| `margin_left` | int | Left margin only |
| `margin_right` | int | Right margin only |
| `font_size` | float | Font size override |
| `icon_size` | float | Icon size override |
| `theme` | string | Icon theme override |
| `slider_track_outline` | float or [float] | 1px outline around slider track |

Colors are specified as a single grayscale value `0.3` or an RGB array `[0.2, 0.2, 0.2]`.

### Layout: Fixed vs Auto

Widgets are laid out in a row. Each widget is either **fixed** or **auto**:

- **Fixed widget**: has `padding` set (either `padding = N` or `padding = "default"` which uses `default_padding` from `[global]`). Its drawn width is `min_width + padding * 2` — it does not grow.
- **Auto widget**: no `padding` field. It absorbs a share of the remaining free space after all fixed widgets and margins are accounted for. Remaining space is divided equally among all auto widgets.

```
|←— touch bar width —→|
| margin |── drawn ──| margin |← next widget →|
         | pad | content | pad |   (fixed)
         |← auto expands →|             (auto)
```

### Layout examples

```toml
# Auto widget — expands to fill remaining space
{ widget = "button", icon = "media-playback-pause", action = { key = ["PlayPause"] } }

# Fixed widget — stays at content size + padding
{ widget = "button", text = "F1", action = { key = ["F1"] }, padding = 5 }

# Fixed widget using global default_padding
{ widget = "battery", mode = "both", padding = "default" }

# Widget with external margin
{ widget = "button", text = "Menu", action = { back = true }, margin = 10 }
```

### Button

```toml
{ widget = "button", text = "F1", action = { key = ["F1"] } }
{ widget = "button", icon = "media-playback-pause", action = { key = ["PlayPause"] } }
{ widget = "button", icon = "activities", action = { panel = "media" } }
{ widget = "button", icon = "konsole", action = { exec = "konsole" } }
```

Fields: `icon` (string), `text` (string), `action` (see Actions below).

### Slider

```toml
{ widget = "slider", slider = "Volume", padding = 300 }
{ widget = "slider", slider = "Brightness", show_outline = false }
{ widget = "slider", slider = "KeyboardBrightness" }
```

Fields:
| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `slider` | string | required | `"Volume"`, `"Brightness"`, or `"KeyboardBrightness"` |
| `icon` | string | none | Icon for the label (instead of text) |
| `text` | string | none | Text for the label (instead of icon) |
| `handle_icon` | string | none | Custom icon for the handle |
| `bar_icon` | string | none | Custom image for the filled bar |
| `handle_size` | int | 20 | Handle diameter in pixels |
| `live_update` | bool | global | Write value during drag vs on release |
| `slider_track_outline` | float or [float] | none | 1px outline around the track pill |

The slider track is drawn as a pill shape with a filled portion (up to the current value) and an empty portion. The handle sits at the boundary. Colors are loaded from the KDE color scheme or GTK theme.

### Battery

```toml
{ widget = "battery", mode = "both" }
{ widget = "battery", mode = "icon" }
{ widget = "battery", mode = "percentage" }
```

Modes: `"icon"`, `"percentage"`, `"both"`. The battery device is auto-detected from `/sys/class/power_supply/`.

### Clock

```toml
{ widget = "clock", format = "%I:%M%P %-e/%m", locale = "en_US" }
```

Format specifiers (strftime-like):
- `%H` — 24-hour (00-23)
- `%I` — 12-hour (01-12)
- `%M` — minute (00-59)
- `%P` — am/pm lowercase
- `%p` — am/pm uppercase
- `%S` — second (00-59)
- `%e` — day of month (1-31, space-padded)
- `%d` — day of month (01-31, zero-padded)
- `%m` — month (01-12)
- `%b` — abbreviated month name
- `%B` — full month name
- `%y` — 2-digit year
- `%Y` — 4-digit year
- `%a` — abbreviated weekday
- `%A` — full weekday
- `%-` — strip padding (e.g. `%-H`)

### Dynamic

```toml
{ widget = "dynamic", show_outline = false }
```

The dynamic widget queries KWin for the active window class and replaces itself with the matching `[app_rules.<class>]` shortcuts. The rules are wrapped in a `Container` that inherits the dynamic widget's common fields (outline, padding, icon_size, font_size, theme, etc.). If no rule matches, the window class name is shown as text.

Requires the KWin script `ktouchbar_dynamicshortcuts` to be installed and enabled.

### Container

```toml
{ widget = "container", widgets = [
    { widget = "button", icon = "document-new", action = { key = ["LeftCtrl", "N"] } },
    { widget = "button", icon = "document-save", action = { key = ["LeftCtrl", "S"] } },
] }
```

Groups children under a shared outline (`show_outline` applies to the group, not individual children). Supports all widget types inside, including nested containers. Children inherit the container's `icon_size`, `font_size`, and `theme` as fallbacks.

Also used internally by the `dynamic` widget.

### Actions

| Action | Syntax | Description |
|--------|--------|-------------|
| Key | `action = { key = ["F1"] }` | Send a single key |
| Key combo | `action = { key = ["LeftCtrl", "LeftShift", "T"] }` | Multi-key shortcut |
| Panel | `action = { panel = "media" }` | Open a sub-panel |
| Exec | `action = { exec = "konsole" }` | Run a command |
| Back | `action = { back = true }` | Return to previous panel |

Key names follow Linux `input-event-codes.h` constants. Common modifiers: `LeftCtrl`, `RightCtrl`, `LeftShift`, `RightShift`, `LeftAlt`, `RightAlt`, `Meta`.

---

## Global config (`[global]`)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `debug` | bool | false | Enable debug output |
| `show_outline` | bool | true | Global widget outline toggle |
| `outline_color` | float/[float] | none | Default outline color |
| `active_color` | float/[float] | none | Default press highlight color |
| `slider_live_update` | bool | false | Write slider value during drag |
| `slider_track_outline` | float/[float] | none | 1px outline around slider track |
| `enable_pixel_shift` | bool | false | Anti-burn-in pixel shifting |
| `font_template` | string | sans-serif | Fontconfig pattern |
| `font_size` | float | 32 | Default font size |
| `icon_size` | float | 48 | Default icon size |
| `icon_theme` | string | auto | Icon theme or "auto" |
| `adaptive_brightness` | bool | true | Follow display brightness |
| `active_brightness` | int | 128 | Active brightness (0-255) |
| `brightness_dim_timeout_ms` | int | 120000 | Idle dim timeout |
| `brightness_off_timeout_ms` | int | 300000 | Idle off timeout |
| `dimmed_brightness` | int | 1 | Dim level (0-255) |
| `panel_timeout_seconds` | int | 5 | Auto-return from panels |
| `default_padding` | int | unset | Default for `padding = "default"` |

---

## File locations

| File | Purpose |
|------|---------|
| `~/.config/ktouchbar/configs/config.toml` | Main config (layers, panels, app rules) |
| `~/.local/share/ktouchbar/icons/` | Custom icon files (.svg, .png) |
| `/usr/share/ktouchbar/configs/` | System defaults |

User config is merged on top of system defaults, with user values taking precedence.
