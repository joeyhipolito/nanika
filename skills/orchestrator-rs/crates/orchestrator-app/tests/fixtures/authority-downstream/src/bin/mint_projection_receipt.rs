use orchestrator_app::{CompatibilityProjection, ProjectionReceipt};

fn main() {
    let _receipt = ProjectionReceipt::compatibility(
        1,
        CompatibilityProjection::Checkpoint,
        "2026-07-16T00:00:00Z",
    );
}
