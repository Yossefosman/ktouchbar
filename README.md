# KTouchBar

A Macbook Touch Bar daemon for KDE Plasma with dynamic shortcuts, expandable menus, and slider controls.

## Features

- **KDE Plasma Integration**: Context-aware buttons that change based on active window/application
- **Dynamic Shortcuts**: KWin script integration for app-specific layouts
- **Expandable Panels**: Multi-level navigation with customizable button groups
- **Slider Controls**: Volume, brightness, and keyboard backlight sliders with live update
- **Container Widget**: Group widgets under a shared outline
- **Custom Commands**: Run any command via button tap

## Notes for T2

Required kernel modules: `apple-bce`, `hid-appletb-kbd`, `hid-appletb-bl`

## Installation (Arch Linux)

```bash
sudo pacman -U ktouchbar-*.pkg.tar.zst
```

Or build from source:

```bash
makepkg -si
```

## Configuration

Config files are in `~/.config/ktouchbar/configs/`:

- **`config.toml`** — Global settings, layers, panels, and app rules

System defaults at `/usr/share/ktouchbar/configs/` are merged with user overrides.

See [share/ktouchbar/WIDGETS.md](share/ktouchbar/WIDGETS.md) for the full widget reference.

## License

KTouchBar is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, version 3 of the License.

This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.

You should have received a copy of the GNU General Public License along with this program. If not, see <https://www.gnu.org/licenses/>.

The original [tiny-dfr](https://github.com/AsahiLinux/tiny-dfr) code remains available under the MIT license (see `LICENSE.mit`).

## Acknowledgments

This project is a fork of [tiny-dfr](https://github.com/AsahiLinux/tiny-dfr) by the Asahi Linux contributors, originally built for the Macbook Touch Bar.
