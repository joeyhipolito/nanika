use orchestrator_app::{IsolatedFixtureRoot, ResolvedRuntimeHome};

fn authorize(resolved: ResolvedRuntimeHome, raw: &IsolatedFixtureRoot) {
    let _ = resolved.authorize_fixture(raw);
}

fn main() {}
