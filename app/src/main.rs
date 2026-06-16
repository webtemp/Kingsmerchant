//! poe2ddd — the application entry point.
//!
//! Launches the quick-mode price-check overlay. Wiring lives in the `overlay`
//! library (Wayland layer surface + egui), which reuses the `parser`,
//! `trade-api`, `platform-linux`, and `ui` crates.

fn main() -> anyhow::Result<()> {
    overlay::run()
}
