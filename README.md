# poe2ddd

[![CI](https://github.com/OWNER/poe2ddd/actions/workflows/ci.yml/badge.svg)](https://github.com/OWNER/poe2ddd/actions/workflows/ci.yml)
[![License: MIT OR GPL-3.0](https://img.shields.io/badge/license-MIT%20OR%20GPL--3.0-blue.svg)](#license)

A native, lightweight Path of Exile 2 price-check overlay for KDE Plasma 6 Wayland.

Press **Ctrl+C** on an item in game and a translucent popup shows the median
asking price and the cheapest listings, each with Whisper / Invite / Hideout /
Trade buttons, a league selector, and an "open on trade site" link. The overlay
takes no keyboard focus (POE2 stays focused) and stays hidden until the first
valid copy.

<!-- Add a screenshot/GIF of the overlay here — it's the highest-value addition
     for the front page. Drop a PNG in `assets/` and reference it. -->

## Requirements

- Linux with **KDE Plasma 6 on Wayland** (uses `wlr-layer-shell`).
- The Rust toolchain (1.96+) to build.
- `xclip` and a running **XWayland** (always present while a Proton game runs).
- Membership in the **`input`** group (see below).

## Running

```sh
cargo run
```

Alt-tab into POE2, hover an item, and press **Ctrl+C**. The league is read from
`~/.config/poe2ddd/config.json` (seeded on first run, switchable from the
selector); `POE_LEAGUE` / `POE_REALM` override it for one run. Set
`RUST_LOG=debug` for detail.

For a packaged install, see [`packaging/arch`](packaging/arch) (Arch PKGBUILD)
and [`assets/INSTALL.md`](assets/INSTALL.md).

## Input access (required)

> [!IMPORTANT]
> Both the global **Ctrl+C** hotkey (evdev read) and chat injection for the
> Whisper/Invite buttons (uinput write) need your user in the `input` group:
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
  KDE Wayland. Requires the `input` group.
- **Clipboard** reads the **X11** CLIPBOARD selection directly (via `xclip`).
  POE2 runs under Proton, so it is an X11/XWayland client and writes the X11
  selection; reading the same XWayland server is the most direct path and avoids
  KWin's flaky X11↔Wayland bridge.
- **Never write the clipboard while testing.** The app only reads, deliberately:
  any process that takes clipboard *ownership* (`wl-copy`, `xclip -i`, a
  clipboard manager) fights KWin's XWayland sync and makes POE2's copies read
  stale/empty. Test with a real in-game copy.

## Architecture

A Cargo workspace split into focused crates, each unit-tested in isolation:

| Crate                   | Responsibility                                                              |
| ----------------------- | -------------------------------------------------------------------------- |
| `app`                   | The `poe2ddd` binary — a thin entry point that launches the overlay.       |
| `crates/parser`         | Parse POE2's "Copy Item" clipboard text into a structured `Item`.          |
| `crates/trade-api`      | Price an `Item` against the official trade API (search/fetch, bulk exchange, rate-limit gating, poeprices.info estimates) over a mockable HTTP seam. |
| `crates/ui`             | The egui price-check view + app logic, windowing-agnostic.                 |
| `crates/overlay`        | Drives the `ui` on focus-less `wlr-layer-shell` surfaces (popup + settings). |
| `crates/platform-linux` | Linux glue: evdev hotkeys, X11 clipboard, uinput chat injection, tray.     |

The network and OS boundaries sit behind traits, so the parsing, query-building,
and rate-limit logic are exercised by ~90 tests without touching the network or
a real Wayland session.

## Development

```sh
cargo test --workspace      # run the full suite
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all             # format
```

CI runs fmt, clippy (`-D warnings`), tests, doc, and an MSRV check on every push
and pull request.

## License

Licensed under either of [MIT](LICENSE-MIT) or
[GPL-3.0-or-later](LICENSE-GPL) at your option.
