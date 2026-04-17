//! Build script for dust-conformance.
//!
//! This build script is intentionally minimal.  The `dust-fixture-minimal`
//! binary is compiled from within the integration tests (see `tests/common/mod.rs`)
//! rather than here, because running `cargo build` from build.rs on the same
//! workspace would deadlock on the Cargo workspace lock.
//!
//! Re-run hints ensure incremental builds work correctly.

fn main() {
    println!("cargo:rerun-if-changed=fixtures/minimal/src/main.rs");
    println!("cargo:rerun-if-changed=fixtures/minimal/Cargo.toml");
}
