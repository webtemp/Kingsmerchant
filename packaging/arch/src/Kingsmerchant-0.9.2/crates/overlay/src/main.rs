//! Thin launcher for the price-check overlay (`cargo run -p overlay`). The real
//! logic lives in the `overlay` library so the top-level `kingsmerchant` binary can
//! share it via [`overlay::run`].

fn main() -> anyhow::Result<()> {
    overlay::run()
}
