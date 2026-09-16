use orchestrator_app::{replace_checkpoint, replace_fixture_event};

fn main() {
    let checkpoint_bypass = std::thread::spawn(|| {
        let _ = replace_checkpoint;
    });
    let event_bypass = std::thread::spawn(|| {
        let _ = replace_fixture_event;
    });
    let _ = checkpoint_bypass.join();
    let _ = event_bypass.join();
}
