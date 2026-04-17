use std::path::PathBuf;

use clap::Parser;
use dust_conformance::{run_section, ConformanceResult};

/// All sections in execution order.
const ALL_SECTIONS: &[&str] = &["handshake", "methods", "heartbeat", "shutdown", "replay"];

/// dust-conform — protocol conformance test runner for dust plugins.
///
/// Spawns the plugin described by PLUGIN_MANIFEST and validates that it follows
/// the dust wire protocol specification.  Each section runs in its own process
/// invocation.
#[derive(Parser, Debug)]
#[command(name = "dust-conform")]
struct Cli {
    /// Path to the plugin's plugin.json manifest file.
    #[arg(long)]
    plugin_manifest: PathBuf,

    /// Run only a named section: handshake, methods, heartbeat, shutdown, replay.
    /// Defaults to running all sections.
    #[arg(long)]
    section: Option<String>,

    /// Emit results as a JSON array to stdout instead of the default text format.
    #[arg(long)]
    json: bool,

    /// Print each section name and result as it runs.
    #[arg(long, short)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    let sections: Vec<&str> = match cli.section.as_deref() {
        Some(s) => vec![s],
        None => ALL_SECTIONS.to_vec(),
    };

    let mut results: Vec<ConformanceResult> = Vec::new();
    let mut all_passed = true;

    for section in &sections {
        if cli.verbose {
            eprintln!("[{section}] running...");
        }

        let result = run_section(&cli.plugin_manifest, section).await;

        if !result.passed {
            all_passed = false;
        }

        if cli.verbose {
            let status = if result.passed { "PASS" } else { "FAIL" };
            eprintln!("[{}] {status} — {}", result.section, result.message);
        }

        results.push(result);
    }

    if cli.json {
        let array: Vec<_> = results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "section": r.section,
                    "passed": r.passed,
                    "message": r.message,
                })
            })
            .collect();

        match serde_json::to_string_pretty(&array) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("error serializing results: {e}"),
        }
    } else {
        let passed_count = results.iter().filter(|r| r.passed).count();
        let total = results.len();
        println!("{passed_count}/{total} sections passed");
        for r in &results {
            let marker = if r.passed { "✓" } else { "✗" };
            println!("  {marker} {}: {}", r.section, r.message);
        }
    }

    if all_passed {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
