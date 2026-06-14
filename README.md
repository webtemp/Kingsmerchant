# poe2ddd

A native, lightweight POE2 price-check overlay for KDE Plasma 6 Wayland.
See [PRD.md](PRD.md) for the full spec and phasing.

## Status: Phase 3 (quick-mode window)

Phases 0–2 done (hotkey→clipboard spike, item parser, trade-API client). Phase 3
is a plain egui window showing quick-mode results: press **Ctrl+C** on an item
in game and the window pops with the median asking price and the cheapest
listings, each with Whisper / Invite / Hideout / Trade buttons. Phase 4 turns
this into a `wlr-layer-shell` overlay at the cursor.

## Running

Requires the Rust toolchain and membership in the `input` group (to read the
keyboard event devices).

```sh
POE_LEAGUE="Runes of Aldur" cargo run -p ui
```

Then alt-tab into POE2, hover an item, and press **Ctrl+C**. The window raises
and prices the item. You can also paste an item or use **Read clipboard**, then
**Price check**. `POE_REALM` (pc/sony/xbox) is optional. Set `RUST_LOG=debug`
for detail.

The Phase 0 stdout spike still lives in `app/` (`cargo run -p poe2ddd`).

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
