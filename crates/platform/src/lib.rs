//! Platform facade for kingsmerchant.
//!
//! `ui` and `overlay` call `platform::foo()` and never name a concrete backend.
//! At compile time this re-exports the backend for the target OS, so the exact
//! same call sites resolve to the Linux or Windows implementation. Adding a new
//! OS means writing a backend that exposes the same surface and wiring one more
//! `cfg` branch here — no churn in the UI code.
//!
//! Both backends MUST expose the same public items (functions and types) with
//! matching signatures; a mismatch surfaces as a build error in `ui`/`overlay`
//! for that target, which is exactly the parity guarantee we want.

#[cfg(target_os = "linux")]
#[allow(clippy::wildcard_imports)]
pub use platform_linux::*;

#[cfg(target_os = "windows")]
#[allow(clippy::wildcard_imports)]
pub use platform_windows::*;

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
compile_error!("kingsmerchant: no platform backend for this target OS (have: linux, windows)");
