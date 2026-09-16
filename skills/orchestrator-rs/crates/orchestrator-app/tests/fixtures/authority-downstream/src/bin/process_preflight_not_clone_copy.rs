use orchestrator_exec::ProcessPreflight;

fn require_clone<T: Clone>() {}
fn require_copy<T: Copy>() {}

fn main() {
    require_clone::<ProcessPreflight<'static>>();
    require_copy::<ProcessPreflight<'static>>();
}
