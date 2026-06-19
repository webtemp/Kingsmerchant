#!/bin/sh
# Install kingsmerchant into ~/.local (binary + .desktop + icon). Run from the repo
# root after `cargo build --release`. See assets/INSTALL.md for details.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bin="$repo_root/target/release/kingsmerchant"

if [ ! -x "$bin" ]; then
    echo "error: $bin not found — run 'cargo build --release' first" >&2
    exit 1
fi

install -Dm755 "$bin" "$HOME/.local/bin/kingsmerchant"
install -Dm644 "$repo_root/assets/kingsmerchant.desktop" \
    "$HOME/.local/share/applications/kingsmerchant.desktop"
install -Dm644 "$repo_root/assets/kingsmerchant.svg" \
    "$HOME/.local/share/icons/hicolor/scalable/apps/kingsmerchant.svg"

update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
kbuildsycoca6 2>/dev/null || true

echo "Installed kingsmerchant to ~/.local."
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo "note: add ~/.local/bin to your PATH to run 'kingsmerchant' directly." ;;
esac
echo "If global hotkeys don't work, add yourself to the 'input' group:"
echo "  sudo usermod -aG input \"\$USER\"   (then log out and back in)"
