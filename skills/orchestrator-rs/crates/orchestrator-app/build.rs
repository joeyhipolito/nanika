use std::{env, io};

const CAPTURED_RUSTC_ENV: &str = "NANIKA_CAPTURED_RUSTC";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=RUSTC");

    let rustc = env::var("RUSTC").map_err(|_| {
        io::Error::other("Cargo did not provide a UTF-8 RUSTC path to orchestrator-app")
    })?;
    if rustc.is_empty() || rustc.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
        return Err(
            io::Error::other("Cargo provided an invalid RUSTC path to orchestrator-app").into(),
        );
    }

    println!("cargo::rustc-env={CAPTURED_RUSTC_ENV}={rustc}");
    Ok(())
}
