use orchestrator_app::ProductionBoundary;

fn bypass(boundary: &ProductionBoundary) {
    let _ = boundary.directory().try_clone();
}

fn main() {}
