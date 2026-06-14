//! Launcher for the Phase 3 quick-mode window.
//!
//! `cargo run -p ui` (or the `poe2-ui` binary). Set `POE_LEAGUE` to a POE2
//! trade league id (default `Runes of Aldur`); `POE_REALM` for pc/sony/xbox.

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    ui::run()
}
