# poe2ddd

A native, lightweight POE2 price-check overlay for KDE Plasma 6 Wayland.
See [PRD.md](PRD.md) for the full spec and phasing.

## Status: quick-mode overlay

A `wlr-layer-shell` overlay that prices the hovered item: press **Ctrl+C** on an
item in game and a translucent popup appears with the median asking price and
the cheapest listings, each with Whisper / Invite / Hideout / Trade buttons, a
league selector, and an "open on trade site" link. The overlay takes no keyboard
focus (POE2 stays focused) and starts hidden until the first valid copy.

## Running

Requires the Rust toolchain and membership in the `input` group (to read the
keyboard event devices).

```sh
cargo run
```

Then alt-tab into POE2, hover an item, and press **Ctrl+C**. The league is read
from `~/.config/poe2ddd/config.json` (seeded on first run, switchable from the
selector); `POE_LEAGUE` / `POE_REALM` override it for one run. Set
`RUST_LOG=debug` for detail.

## Input access (required)

> [!IMPORTANT]
> Both the global **Ctrl+C** hotkey (evdev read) and chat injection for the
> Whisper/Invite buttons (uinput write) need your user to be in the `input`
> group:
>
> ```sh
> sudo usermod -aG input "$USER"   # then log out and back in
> ```
>
> Without this the hotkey **silently does nothing** — no error, no popup. This
> is the #1 "it doesn't work" cause.

## Platform notes

- **Hotkeys** read `/dev/input/by-id/*-event-kbd` directly via evdev — there is
  no usable compositor global-shortcut path for an XWayland-targeted overlay on
  KDE Wayland (PRD §4.1). You must be in the `input` group.
- **Clipboard** reads the **X11** CLIPBOARD selection directly (via `xclip`),
  not the Wayland clipboard the PRD §4.2/§9.2 assumed. POE2 runs under Proton,
  so it is an X11/XWayland client and writes the X11 selection; reading the same
  XWayland server is the most direct path and avoids KWin's flaky X11↔Wayland
  bridge. Requires `xclip` and a running XWayland (`DISPLAY` set — it always is
  when a Proton game is running).
- **Never write the clipboard while testing.** The app only reads, and that's
  deliberate: any process that takes clipboard *ownership* (`wl-copy`,
  `xclip -i`, a clipboard manager) fights KWin's XWayland sync and makes POE2's
  copies read stale/empty (PRD §4.2). Test with a real in-game copy, not by
  injecting clipboard contents from another tool.
