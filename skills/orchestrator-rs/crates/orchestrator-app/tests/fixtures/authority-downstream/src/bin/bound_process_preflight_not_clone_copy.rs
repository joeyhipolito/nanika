use orchestrator_exec::BoundProcessPreflight;

fn require_clone<T: Clone>() {}
fn require_copy<T: Copy>() {}

fn main() {
    require_clone::<BoundProcessPreflight>();
    require_copy::<BoundProcessPreflight>();
}
