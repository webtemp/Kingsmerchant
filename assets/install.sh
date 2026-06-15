#!/bin/sh
# Install poe2ddd into ~/.local (binary + .desktop + icon). Run from the repo
# root after `cargo build --release`. See assets/INSTALL.md for details.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bin="$repo_root/target/release/poe2ddd"

if [ ! -x "$bin" ]; then
    echo "error: $bin not found — run 'cargo build --release' first" >&2
    exit 1
fi

install -Dm755 "$bin" "$HOME/.local/bin/poe2ddd"
install -Dm644 "$repo_root/assets/poe2ddd.desktop" \
    "$HOME/.local/share/applications/poe2ddd.desktop"
install -Dm644 "$repo_root/assets/poe2ddd.svg" \
    "$HOME/.local/share/icons/hicolor/scalable/apps/poe2ddd.svg"

update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
kbuildsycoca6 2>/dev/null || true

echo "Installed poe2ddd to ~/.local."
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo "note: add ~/.local/bin to your PATH to run 'poe2ddd' directly." ;;
esac
echo "If global hotkeys don't work, add yourself to the 'input' group:"
echo "  sudo usermod -aG input \"\$USER\"   (then log out and back in)"
