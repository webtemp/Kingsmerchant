# poe2-pricer

A native, lightweight POE2 price-check overlay for KDE Plasma 6 Wayland.
See [PRD.md](PRD.md) for the full spec and phasing.

## Status: Phase 0 (spike)

Detect a global Ctrl+C / Ctrl+Alt+C, read the clipboard the game just wrote,
print the item text to stdout. No parsing, trade API, or UI yet.

## Running the spike

Requires the Rust toolchain and membership in the `input` group (to read the
keyboard event devices).

```sh
cargo run
```

Then alt-tab into POE2, hover an item, and press **Ctrl+C** (quick) or
**Ctrl+Alt+C** (detailed). The copied item text prints to the terminal.

Set `RUST_LOG=debug` for per-copy timing.

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
