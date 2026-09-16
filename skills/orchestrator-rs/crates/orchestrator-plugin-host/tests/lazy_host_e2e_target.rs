#![cfg(unix)]

#[path = "../src/bin/fake-orchestrator-plugin.rs"]
mod fake_plugin;
#[path = "../src/lib.rs"]
mod orchestrator_plugin_host;

fn main() -> std::process::ExitCode {
    let loopback_address = std::env::args()
        .nth(1)
        .and_then(|argument| argument.parse::<std::net::SocketAddr>().ok())
        .is_some_and(|address| address.ip().is_loopback());
    if loopback_address {
        return fake_plugin::fixture_main();
    }

    match orchestrator_plugin_host::tests::run_all() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lazy_host_e2e failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
