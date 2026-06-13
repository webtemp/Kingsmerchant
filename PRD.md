# PRD: poe2-pricer (working name)

A native, lightweight POE2 price-check overlay for KDE Plasma 6 Wayland.
Windows support is a future phase. POE1 support is a future phase.

## 1. Why

The existing tool ("Exiled Exchange 2") is Electron-based and depends on
X11-only native addons (`uiohook-napi`, `electron-overlay-window`). Making it
work on KDE Wayland required heavy patches, the result is opaque/fullscreen,
and the Electron runtime is heavy. We want a small native binary that does
one thing well.

## 2. Scope of v1

### In scope
- KDE Plasma 6 Wayland session, Arch Linux. POE2 launched via Steam/Proton
  (so the game window is XWayland from the compositor's POV).
- Two hotkeys, two modes:
  - **Ctrl+C** — quick mode: median asking price + cheapest 5 active listings.
  - **Ctrl+Alt+C** — detailed mode: full breakdown (stat filters, price range,
    similar-item filter, listings with seller actions).
- Settings UI window (separate from the price-check popup).
- Tray icon + `.desktop` entry so the app appears in the KDE launcher.
- Single binary + `config.json` next to it. Hand-editable.

### Explicitly out of scope for v1
- Windows support (planned next phase).
- POE1 support (planned next phase).
- All secondary EE2 widgets: XP tracker, image strip, library, delve grid,
  stash search.
- Patreon banner / support-the-dev UI.
- Heist OCR / vision worker.
- Setup wizard / first-run "point me at production_Config.ini" flow — auto-detect
  what we can, ask for the rest in Settings.
- Auto-update — manual download for v1.

## 3. Core user story

> "I'm playing POE2. I hover over a rare item in my inventory, press Ctrl+C,
> and within ~2 seconds a popup appears next to my cursor showing the median
> asking price and the five cheapest live listings. I press Ctrl+Alt+C on the
> same item and the popup expands into a detailed view with filterable stats.
> I click 'Whisper' on a listing and the `@seller wtb …` line is on my
> clipboard ready to paste into POE2 chat."

If this works end-to-end, v1 ships.

## 4. Functional requirements

### 4.1 Hotkey detection
- Global hotkeys: **Ctrl+C** (quick) and **Ctrl+Alt+C** (detailed) must fire
  whether or not the app window has focus.
- Bound to POE2's own copy combos on purpose — POE2 itself does the copy, the
  app just reads the resulting clipboard. (Synthesizing keys into XWayland on
  KDE Plasma 6 is blocked by the compositor; this design sidesteps that.)
- Implementation: read `/dev/input/by-id/*-event-kbd` directly. User must be in
  the `input` group. The default Node/libuv-style threadpool defaults bite if
  more than 4 keyboards are connected — set threadpool to ≥16 in the runtime
  (Rust/tokio: not an issue, but document it).

### 4.2 Clipboard read
- After hotkey fires, read clipboard until either:
  - content changed AND content is recognized as a POE2 item → proceed
  - timeout of 500 ms → abort (no popup)
- **Do not clear the clipboard before polling**. On KDE Wayland's XWayland,
  once Electron/our app takes selection ownership, POE2 can't reliably take
  it back, so the next press would never see a new item.
- Side effect of "no clear": hovering the same item twice doesn't refresh —
  acceptable, document it.
- Use `wlr-data-control` protocol (via `wl-clipboard-rs`) for the Wayland
  clipboard — works without window focus.

### 4.3 Item parsing
- Parse POE2 item text (the exact format the game writes to clipboard on
  Ctrl+C / Ctrl+Alt+C). Supports: rares, uniques, currency, fragments,
  jewels, gems, maps, waystones.
- Maintain a snapshot of stat definitions from
  `https://www.pathofexile.com/api/trade2/data/stats` and item definitions
  from `https://www.pathofexile.com/api/trade2/data/items`.
- Refresh these snapshots on app start (~once per launch is fine).
- Parser is a pure-logic library with snapshot tests against real
  game-copied item strings.

### 4.4 Trade API
- Primary: official trade API.
  - `POST https://www.pathofexile.com/api/trade2/search/{league}` to submit
    a query, get a `queryId` + list of `result` ids.
  - `GET https://www.pathofexile.com/api/trade2/fetch/{ids}?query={queryId}`
    to fetch listing details in batches of up to 10 ids.
- Secondary: `https://www.poeprices.info/api?…` for ML-based price prediction
  on rares where the official exact-match search is too narrow. Used only in
  detailed mode.
- **Rate limits**: track every response's `X-Rate-Limit-*` and
  `X-Rate-Limit-State-*` headers. Maintain per-bucket counters
  (per-second / per-minute / per-hour). If a new request would breach a
  bucket, queue it and show "rate limited, retrying in Ns" in the popup;
  don't fire blindly. Same approach EE2 / awakened-poe-trade use.
- No auth in v1 (anonymous queries work for search).

### 4.5 Popup window
- **One small, transparent, always-on-top window per popup** (not one big
  fullscreen container — that was Electron-EE2's mistake on Wayland).
- Wayland: `wlr-layer-shell` surface in the `overlay` layer, anchored by
  pixel coords. Input region = the popup's bounds (so clicks outside pass
  through naturally — no `setIgnoreMouseEvents` toggling).
- Position modes (toggle in Settings):
  - **At cursor** (default) — pop next to where the mouse is when Ctrl+C
    fires; clamp to screen edges.
  - **Fixed** — pop at a configured screen-relative position.
- Either mode: **Alt + drag** moves the popup. Last position is remembered
  per mode.
- Dismiss: **Esc** closes; next Ctrl+C on a different item *replaces* the
  popup.
- Multi-monitor: popup appears on the monitor containing the cursor at
  press-time.

### 4.6 Quick mode UI
- Item name + rarity icon.
- Median asking price (in chaos / divine / exalted as appropriate).
- 5 cheapest listings:
  - asking price
  - seller (account or character name)
  - listed-at age ("3m ago")
  - buttons: **Whisper / Invite / Hideout / Trade-with** — each copies the
    appropriate chat command (`@seller wtb …`, `/invite seller`,
    `/hideout seller`, `/tradewith seller`) to the clipboard. User pastes
    into POE2 chat manually. (We can't auto-paste into XWayland on KDE
    Wayland.)

### 4.7 Detailed mode UI
- Everything from quick mode, plus:
  - Per-stat filter toggles (on/off, range sliders).
  - Price range filter.
  - "Similar item" filter (same base + similar implicit/explicit set).
  - Live re-query when filters change (debounced, respects rate limits).
  - poeprices.info ML estimate badge on rares.

### 4.8 Settings UI
- Separate window, opened from tray icon or from a gear button on the popup.
- Fields (v1):
  - **League** — dropdown, populated from
    `https://www.pathofexile.com/api/leagues?type=main&realm=poe2`.
  - **Realm** — pc / sony / xbox (anonymous queries are realm-aware).
  - **Position mode** — at-cursor / fixed.
  - **Fixed position** — x, y, monitor (if fixed mode).
  - **Hotkeys** — quick / detailed / close-popup (Ctrl+C / Ctrl+Alt+C / Esc
    are defaults but rebindable).
  - **Log keys** — toggle for debugging.
- Settings live in `./config.json` next to the binary. Manually editable.
  Hot-reload on save (`inotify` / file watcher).

### 4.9 Tray icon
- KDE-friendly StatusNotifierItem (SNI), not the legacy XEmbed tray.
- Menu: Open Settings, Quit.
- Tooltip shows app state ("Listening" / "Rate limited 4s" / "API error").

## 5. Non-functional requirements

- **Latency budget**: hotkey to popup-rendered ≤ 2 s on a warm app
  (excluding GGG API latency itself, which is typically 200-800 ms).
- **Binary size**: target ≤ 20 MB stripped.
- **Memory**: target ≤ 100 MB RSS at idle.
- **No process-memory reads of POE2.** No screen capture. (Anti-cheat sees
  these. Clipboard + window position only.)
- **No telemetry.** No phone-home for v1.
- **License**: GPL-3.0 or MIT, user's choice (no PRD opinion).

## 6. Architecture

Single Rust binary. Three async tasks talking over `tokio::mpsc` channels.

```
┌─────────────────────────────────────────────────────────┐
│                       Main task                          │
│              (egui, ui state, settings)                  │
└────────┬─────────────────────────┬─────────────┬────────┘
         │                         │             │
   ┌─────▼─────┐         ┌─────────▼────────┐  ┌─▼──────────┐
   │ Input task│         │ Game-window task │  │ Trade-API  │
   │ (evdev)   │         │ (xdotool poll)   │  │ (reqwest)  │
   └───────────┘         └──────────────────┘  └────────────┘
```

### Stack (Rust)

| Concern | Crate |
|---|---|
| UI | `eframe` / `egui` (multi-window via raw `winit` + `egui-winit`) |
| Wayland overlay | `smithay-client-toolkit` for `wlr-layer-shell` |
| HTTP | `reqwest` (with `rustls`, not `openssl`) |
| Async runtime | `tokio` |
| Clipboard | `wl-clipboard-rs` |
| Global hotkeys | hand-rolled `evdev` reader on `/dev/input/by-id/*-event-kbd` |
| Game window pos | shell out to `xdotool` (POE2 is XWayland) |
| Config | `serde` + `serde_json` + `notify` for hot-reload |
| Tray | `ksni` (KDE StatusNotifierItem) |
| Logging | `tracing` + `tracing-subscriber` |
| Errors | `thiserror` for libs, `anyhow` for the binary |

### Project layout (Cargo workspace)

```
poe2-pricer/
├── Cargo.toml                  # workspace
├── crates/
│   ├── parser/                 # pure logic: item-text → struct
│   ├── trade-api/              # HTTP client + rate-limit bucket tracker
│   ├── platform-linux/         # evdev, layer-shell, xdotool, wl-clipboard
│   └── ui/                     # egui app, popup + settings windows
├── app/
│   └── src/main.rs             # wiring
├── assets/                     # icon, .desktop template
└── tests/                      # end-to-end harness
```

## 7. Test plan

### Unit
- `parser`: snapshot tests against real game-copied item strings. Every
  rarity / type / language has fixtures in `tests/items/`.
- `trade-api`: request-builder tests (stat-id mappings, league string,
  query merging). Response parser tests against recorded JSON fixtures in
  `tests/fixtures/api/`.
- Rate-limit bucket: simulated header sequences → expected wait times.

### Integration
- Mock clipboard + mock trade-API → fire synthetic hotkey event → assert
  the popup widget would receive the right item + listings.

### End-to-end (manual, scripted where possible)
- Run against the real POE2 client on the dev machine. Before each release:
  1. Hover rare ring, Ctrl+C → popup ≤ 2 s with median + 5 listings.
  2. Same item, Ctrl+Alt+C → detailed mode with stat filters.
  3. Toggle a stat filter → listings re-query within 500 ms (or queued
     with a visible "throttled" message).
  4. Whisper button on a listing → clipboard contains `@seller wtb …`.
  5. Alt-drag the popup → it moves; close → reopen → it remembers.
  6. Disconnect network mid-query → graceful error in popup, not a crash.

### CI
- GitHub Actions on `ubuntu-latest` (24.04+):
  - `cargo test --workspace`
  - `cargo clippy --workspace -- -D warnings`
  - `cargo audit`
  - Build release binary, upload as artifact.
- Tag push (`v*`) → build release binary + checksum, publish a GitHub Release.

## 8. Phasing

Each phase ships something demoable.

| Phase | Goal | ETA (evenings) |
|---|---|---|
| 0 | Spike: detect global Ctrl+C, read clipboard, print item text to stdout | 1 weekend |
| 1 | Item parser, snapshot tests, no UI | 1 weekend |
| 2 | Trade API client, recorded fixtures, rate-limit buckets | 1 weekend |
| 3 | Plain egui window (not overlay yet) showing quick-mode results | 1 weekend |
| 4 | Convert to layer-shell overlay, position at cursor, Alt-drag | 1 weekend |
| 5 | Detailed mode + filters | 1-2 weekends |
| 6 | Settings UI, tray icon, .desktop entry, config hot-reload | 1 weekend |
| 7 | Polish: error states, rate-limit UI, multi-monitor, packaging | 1 weekend |

Total: ~8-10 evenings of focused work for a solo dev.

## 9. Known platform constraints (lessons from Electron-EE2 debug)

These should be treated as fixed external facts, not solved problems:

1. **Cannot synthesize keys into POE2 on KDE Wayland.** `ydotool` events
   reach the kernel but the compositor doesn't forward synthetic input to
   XWayland clients. Design accordingly — never try to type into POE2.
2. **Clipboard requires the `wlr-data-control` protocol** for reading
   without window focus. Don't rely on focus-gated clipboard APIs.
3. **POE2 hover-detection is mouse-motion-gated** — if the cursor has been
   static, the first Ctrl+C often does nothing. This is the game's behavior,
   not ours. Don't try to fix it; mention it in the README.
4. **xdotool works for POE2** because POE2 runs under XWayland (Proton).
   Native Wayland games would need a different approach but POE2 is fine.
5. **Multiple keyboards + Node-style threadpools.** Doesn't apply to Rust/
   tokio but applies if anyone reuses our evdev approach in Node.

## 10. Future phases (post-v1, not committed)

- **v1.1**: Windows support. Per-platform input/window/clipboard impls
  swapped in via the trait already drawn in v1. UI/parser/trade-api are
  shared.
- **v1.2**: POE1 league + trade API (same shape, different endpoints).
- **v1.3**: OAuth ("Login with PoE Account") so we can show your own listings.
- **v1.4**: Stash search, paste-in-chat helpers, hideout teleport quick-bar.
- **v1.5**: Auto-update via GitHub Releases.
- **v1.6**: Flatpak + AppImage for redistribution.

---

## Appendix A — reference repos (read, don't copy)

- **Exiled Exchange 2** (Electron, current tool) — for trade-API request shapes,
  item-parsing edge cases, league/realm handling.
- **awakened-poe-trade** (POE1 ancestor) — for rate-limit bucket logic, parser
  fixtures.

## Appendix B — anti-cheat note

POE2 ships with anti-cheat (BattlEye). All known POE overlays operate
exclusively on the clipboard + window position. They do not read game memory
or inject DLLs. We do the same. As long as we never go further, BattlEye is
not in play.
