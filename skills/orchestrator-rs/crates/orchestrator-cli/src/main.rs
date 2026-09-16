use std::{
    env,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    // Hidden canary worker mode: the enrolled current executable consumes the
    // durable parent's exact stdin challenge and emits a PID-bound proof. The
    // worker itself has no filesystem side effects.
    #[cfg(all(unix, feature = "verification-process-canary"))]
    {
        let args: Vec<String> = env::args().collect();
        if args.len() == 2 && args[1] == "--hermetic-canary-worker" {
            return orchestrator_cli::run_hermetic_canary_worker();
        }
    }

    let arguments = env::args().skip(1);
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut output = stdout.lock();
    let mut error_output = stderr.lock();
    match orchestrator_cli::run_system(arguments, &mut output, &mut error_output) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(error_output, "{error}");
            ExitCode::FAILURE
        }
    }
}
