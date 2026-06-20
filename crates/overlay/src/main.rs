//! Thin launcher for the price-check overlay; logic lives in [`overlay::run`].

fn main() -> anyhow::Result<()> {
    overlay::run()
}
