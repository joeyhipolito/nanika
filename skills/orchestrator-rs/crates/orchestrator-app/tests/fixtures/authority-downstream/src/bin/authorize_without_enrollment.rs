use orchestrator_app::ResolvedRuntimeHome;

fn authorize(resolved: ResolvedRuntimeHome) {
    let _ = resolved.authorize_production();
}

fn main() {}
