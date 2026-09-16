use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut output = stdout.lock();
    let mut error_output = stderr.lock();
    let code = orchestrator_first_use_pilot::main_with(
        std::env::args().skip(1),
        std::env::var(orchestrator_first_use_pilot::OPT_IN_ENV).ok(),
        &mut output,
        &mut error_output,
    );
    let _ = output.flush();
    ExitCode::from(code)
}
