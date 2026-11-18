# darmoshark-battery-tray (Windows)

A lightweight, zero-runtime-dependency Windows system tray application written in Rust (`windows-sys`) that displays your Darmoshark mouse battery percentage directly in the taskbar notification area.

## Features

- **Dynamic tray icon**: renders the battery percentage (`0`–`100` or `--`) in the system tray with color-coded indicator bar (green > 50%, yellow 21–50%, red ≤ 20%, gray when disconnected).
- **Tooltip & status**: hover to see the detected model and battery percentage (including `(asleep)` when a 2.4 GHz receiver is plugged in while the mouse is sleeping).
- **Auto-detection**: listens for USB/HID device arrival and removal (`WM_DEVICECHANGE`) and polls every 60 seconds.
- **Context menu** (right-click):
  - Device & battery status
  - **Refresh now** (also triggered by left-clicking the tray icon)
  - **Start with Windows** (toggles `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`)
  - **Exit**

## Build on Windows

With Rust installed (`rustup` MSVC or GNU toolchain):

```powershell
cd windows
cargo build --release
```

The executable will be at `windows/target/release/darmoshark-battery-tray.exe`.

## Cross-compile from Linux

Install `mingw-w64` (e.g. `sudo pacman -S mingw-w64-gcc` on Arch or `sudo apt install gcc-mingw-w64-x86-64` on Debian/Ubuntu) and the Windows GNU target:

```sh
rustup target add x86_64-pc-windows-gnu
cd windows
cargo build --release --target x86_64-pc-windows-gnu
```

## Run protocol tests

```sh
cd windows
cargo test
```
