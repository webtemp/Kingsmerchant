# Kingsmerchant

[![CI](https://github.com/webtemp/Kingsmerchant/actions/workflows/ci.yml/badge.svg)](https://github.com/webtemp/Kingsmerchant/actions/workflows/ci.yml)
[![License: MIT OR GPL-3.0](https://img.shields.io/badge/license-MIT%20OR%20GPL--3.0-blue.svg)](#license)

**A fast, native Path of Exile 2 price-check overlay for Linux — built for KDE Plasma 6 on Wayland.**

Hover an item in game, press **Ctrl+C**, and a translucent popup shows the
median asking price and the cheapest live listings — each with one-click
Whisper / Invite / Hideout / Trade actions. No Electron, no browser, no stolen
keyboard focus: POE2 stays focused the whole time, and the overlay stays hidden
until your first copy.

<!-- Highest-value addition for the front page: drop a screenshot/GIF of the
     overlay in action into assets/ and reference it here, e.g.:
     ![Kingsmerchant overlay](assets/screenshot.png) -->

## Features

- **Instant price check** — Ctrl+C on a hovered item shows the median price and
  the cheapest listings, sampled live from the official trade API.
- **One-click trade actions** — Whisper, Invite, Hideout, and Trade buttons per
  listing (the chat command is copied to your clipboard, since Wayland blocks
  typing into POE2). Instant-Buyout listings also get a **Teleport to hideout**
  button when a `POESESSID` is configured.
- **Detailed stat filters** — a live filter panel with a toggle per mod,
  per-mod minimum rolls, a price range, rarity and resistance handling, and
  miscellaneous flags. Edits re-run the search automatically.
- **Bulk currency exchange** — stackables (currency, runes, …) are priced
  through the bulk exchange, backed by the poe2scout economy with the official
  exchange as a fallback.
- **ML price estimate** — a poeprices.info machine-learning estimate badge for
  rares, alongside the live-listings median.
- **Theme manager** — customise the accent colours and popup opacity from
  Settings (colour pickers + an opacity slider) or by hand in `config.json`,
  with four built-in presets (Default Gold, Minimal Slate, Crimson Ember,
  Arcane Violet). See [Configuration](#configuration).
- **Craft of Exile link** — open the current item directly in the
  [Craft of Exile](https://www.craftofexile.com/?game=poe2) crafting simulator.
- **Open on trade site** — deep-link the exact search (every filter included) to
  the official trade site.
- **League aware** — auto-resolves the current league at startup and lets you
  pin one from the selector; follows league rollovers until you pick.
- **Native & lightweight** — a small Rust binary drawing on a focus-less
  `wlr-layer-shell` surface. It takes no keyboard focus and stays hidden until
  the first valid copy.

## Requirements

- Linux with **KDE Plasma 6 on Wayland** (uses `wlr-layer-shell`).
- **`xclip`** and **`xdotool`** (and a running **XWayland**, always present while
  a Proton game runs). `xclip` reads POE2's clipboard; `xdotool` detects the
  focused POE2 window for the hotkey gate and popup placement. Without
  `xdotool` the Ctrl+C gate never fires and nothing happens.
- **`xdg-utils`** (`xdg-open`) for the trade-site / Craft of Exile links.
- Membership in the **`input`** group (see [Input access](#input-access-required)).
- The Rust toolchain (**1.96+**) to build from source.

## Install

### Arch Linux

A PKGBUILD ships in [`packaging/arch`](packaging/arch); see
[`assets/INSTALL.md`](assets/INSTALL.md) for the full walkthrough.

### From source

```sh
cargo run --release
```

`cargo run` launches the overlay (the `kingsmerchant` binary is the workspace
default). Alt-tab into POE2, hover an item, and press **Ctrl+C**.

`POE_LEAGUE` / `POE_REALM` override the configured league/realm for a single run;
`RUST_LOG=debug` turns on detailed logging.

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

## Usage

| Hotkey            | Action                                             |
| ----------------- | -------------------------------------------------- |
| **Ctrl+C**        | Price-check the hovered item (opens the popup)     |
| **F5**            | Run the hideout chat macro (`/hideout` by default) |
| **F2**            | Run the second chat macro (`/exit` by default)     |
| **Escape**        | Close the popup                                    |
| **Ctrl+Alt+drag** | Move the popup; where you drop it is remembered    |

All hotkeys are rebindable in **Settings** (the gear icon, or the tray menu).
By default they only fire while POE2 is the focused window, so Ctrl+C elsewhere
isn't hijacked.

## Configuration

Settings are stored at `~/.config/kingsmerchant/config.json` (honouring
`XDG_CONFIG_HOME`). It's seeded on first run, editable from the in-app Settings
panel, and **hot-reloaded** when changed on disk — so hand edits apply live.

Notable fields:

| Field                      | Meaning                                                       |
| -------------------------- | ------------------------------------------------------------ |
| `league` / `league_pinned` | Trade league; empty + unpinned = auto-resolve at startup     |
| `trade_status`             | Which listings to search (`securable` / `online` / …)        |
| `filter_min_percent`       | How tightly per-mod filter minimums are seeded from the roll |
| `hotkey_*`                 | Rebindable hotkeys (e.g. `"Ctrl+C"`, `"F5"`, `"Escape"`)     |
| `poesessid`                | Trade-site session cookie — only for the Teleport button     |
| `theme`                    | Accent colours + popup opacity (see below)                   |

### Theme

The `theme` block holds `#rrggbb` accent colours and an `opacity`
(`0.0`–`1.0`, lower = more see-through to the game). Defaults reproduce the
original look; the rarity/frame colours are fixed because they mirror the
in-game item colours.

```jsonc
"theme": {
  "accent_gold":    "#e6c25a",  // headline price / accents
  "affix_blue":     "#8a8af0",  // rolled-mod text
  "online_dot":     "#4cd137",  // online / valid indicator
  "header_bg":      "#17171c",  // inset item cards
  "overlay_fill":   "#2c2e36",  // popup background
  "overlay_stroke": "#50525e",  // popup border
  "opacity": 1.0
}
```

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
| `app`                   | The `kingsmerchant` binary — a thin entry point that launches the overlay. |
| `crates/parser`         | Parse POE2's "Copy Item" clipboard text into a structured `Item`.          |
| `crates/trade-api`      | Price an `Item` against the official trade API (search/fetch, bulk exchange, rate-limit gating, poeprices.info estimates) over a mockable HTTP seam. |
| `crates/ui`             | The egui price-check view + app logic, windowing-agnostic.                 |
| `crates/overlay`        | Drives the `ui` on focus-less `wlr-layer-shell` surfaces (popup + settings). |
| `crates/platform-linux` | Linux glue: evdev hotkeys, X11 clipboard, uinput chat injection, tray.     |

The network and OS boundaries sit behind traits, so the parsing, query-building,
and rate-limit logic are exercised by the test suite without touching the
network or a real Wayland session.

## Development

```sh
cargo test --workspace                                  # run the full suite
cargo clippy --workspace --all-targets -- -D warnings   # lint
cargo fmt --all                                         # format
```

CI runs fmt, clippy (`-D warnings`), tests, doc, and an MSRV check on every push
and pull request.

## Disclaimer

Kingsmerchant is an unofficial, fan-made tool. It is not affiliated with,
endorsed by, or associated with Grinding Gear Games. Path of Exile is a
trademark of Grinding Gear Games.

## License

Licensed under either of [MIT](LICENSE-MIT) or
[GPL-3.0-or-later](LICENSE-GPL) at your option.
