const LOCKFILE: &str = include_str!("../../../Cargo.lock");
const PROHIBITED_JAVASCRIPT_RUNTIMES: [&str; 4] = ["bun", "bunx", "node", "nodejs"];

#[test]
fn locked_native_graph_has_no_javascript_runtime_package() {
    for package in PROHIBITED_JAVASCRIPT_RUNTIMES {
        let lock_entry = format!("name = \"{package}\"");
        assert!(
            !LOCKFILE.lines().any(|line| line == lock_entry),
            "prohibited installed/runtime dependency: {package}"
        );
    }
}

#[cfg(unix)]
#[test]
fn root_help_does_not_launch_javascript_runtimes() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let root = std::env::temp_dir().join(format!(
        "orchestrator-rs-runtime-policy-{}",
        std::process::id()
    ));
    let bin_dir = root.join("bin");
    let marker = root.join("javascript-runtime-launched");
    std::fs::create_dir_all(&bin_dir)?;

    for runtime in PROHIBITED_JAVASCRIPT_RUNTIMES {
        let stub = bin_dir.join(runtime);
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' {runtime} >> \"{}\"\n",
                marker.display()
            ),
        )?;
        let mut permissions = std::fs::metadata(&stub)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&stub, permissions)?;
    }

    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .arg("--help")
        .env("PATH", &bin_dir)
        .output()?;
    assert!(output.status.success(), "root help command failed");

    assert!(!marker.exists(), "a JavaScript runtime stub was executed");
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn unsupported_cli_arguments_are_not_reflected_to_stderr() -> Result<(), Box<dyn std::error::Error>>
{
    use std::process::Command;

    const SENTINEL: &str = "AUDIT_SENTINEL_NOT_A_SECRET";
    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .args(["daemon", "start", "--api-key", SENTINEL])
        .output()?;

    assert!(!output.status.success());
    assert!(!String::from_utf8(output.stderr)?.contains(SENTINEL));
    Ok(())
}

#[test]
fn contract_summary_diagnostic_is_not_exposed_by_the_binary()
-> Result<(), Box<dyn std::error::Error>> {
    use std::process::Command;

    let output = Command::new(env!("CARGO_BIN_EXE_orchestrator"))
        .arg("contract-summary")
        .output()?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "unknown command \"contract-summary\" for \"orchestrator\"\n"
    );
    Ok(())
}
