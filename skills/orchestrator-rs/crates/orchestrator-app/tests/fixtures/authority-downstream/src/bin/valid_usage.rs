use orchestrator_app::ProcessSupervisor;

fn main() {
    assert!(ProcessSupervisor::new(1).is_ok());
}
