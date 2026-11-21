pub mod cli;
pub mod common;
pub mod iidx;
pub mod popn;
pub mod run;
pub mod sdvx;

/// Displayed version: overridden at release build time via `UNIGST_VERSION`
/// (e.g. `2026.11.21.1` from tag `v2026.11.21.1`), falling back to `Cargo.toml`.
pub const VERSION: &str = match option_env!("UNIGST_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
