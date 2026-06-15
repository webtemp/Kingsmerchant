# Installing poe2ddd (KDE Plasma 6 Wayland)

This installs the binary, the launcher entry, and the icon into your user
prefix (`~/.local`) — no root needed. Run from the repo root.

## 1. Build the release binary

```sh
cargo build --release
# → target/release/poe2ddd
```

## 2. Install (binary + .desktop + icon)

The helper script does all three:

```sh
./assets/install.sh
```

Or do it by hand:

```sh
# Binary onto your PATH
install -Dm755 target/release/poe2ddd ~/.local/bin/poe2ddd

# Launcher entry + icon
install -Dm644 assets/poe2ddd.desktop ~/.local/share/applications/poe2ddd.desktop
install -Dm644 assets/poe2ddd.svg \
  ~/.local/share/icons/hicolor/scalable/apps/poe2ddd.svg

# Refresh the launcher / icon caches (so KDE picks them up)
update-desktop-database ~/.local/share/applications 2>/dev/null || true
kbuildsycoca6 2>/dev/null || true
```

Make sure `~/.local/bin` is on your `PATH` (most distros add it automatically).
If you keep the binary elsewhere, edit the `Exec=` line in the installed
`poe2ddd.desktop` to the absolute path.

## 3. One-time: input group (global hotkeys)

poe2ddd reads keyboards directly via evdev for the global Ctrl+C hotkey
(PRD §4.1), which needs membership in the `input` group:

```sh
sudo usermod -aG input "$USER"
# then log out and back in for it to take effect
```

## 4. Run

Launch **poe2ddd** from the KDE app launcher (or just run `poe2ddd`). It starts
hidden with a tray icon (a gold diamond). Hover an item in POE2 and press
**Ctrl+C** to pop the price check; open **Settings** from the tray menu or the
gear button on the popup. Settings live in `~/.config/poe2ddd/config.json` and
hot-reload when you edit them (hotkey/realm/focus changes need a restart).
