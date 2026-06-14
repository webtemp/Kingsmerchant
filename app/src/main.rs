//! poe2ddd — the application entry point.
//!
//! Launches the quick-mode price-check overlay (PRD §4.5). All the wiring lives
//! in the `overlay` library (Wayland layer surface + egui), which reuses the
//! `parser`, `trade-api`, `platform-linux`, and `ui` crates.
//!
//! `cargo run` (this binary) is the normal way to start poe2ddd.

fn main() -> anyhow::Result<()> {
    overlay::run()
}
