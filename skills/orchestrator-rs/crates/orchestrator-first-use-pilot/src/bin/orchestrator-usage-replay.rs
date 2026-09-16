//! Read-only saved Claude usage replay CLI.
use std::path::Path;

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!(
            "usage: orchestrator-usage-replay --input <saved-Claude.jsonl> --output <new-report.json>"
        );
        return std::process::ExitCode::SUCCESS;
    }
    if args.len() != 4 || args[0] != "--input" || args[2] != "--output" {
        eprintln!(
            "usage: orchestrator-usage-replay --input <saved-Claude.jsonl> --output <new-report.json>"
        );
        return std::process::ExitCode::from(2);
    }
    match orchestrator_first_use_pilot::replay_usage(Path::new(&args[1]), Path::new(&args[3])) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("usage replay: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
